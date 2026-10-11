//! `dev`: rebuild and restart the application when its sources change.
//!
//! [`Supervisor`] is the build/run state machine. Every change event
//! starts a build; only a successful build replaces the running app, so a
//! compile error leaves the last healthy version serving. Replacing stops
//! the old process (SIGTERM to its process group, a grace period, then
//! SIGKILL) and waits for it to exit before the new one starts, so the
//! port is free. Events that arrive during a build are coalesced into one
//! follow-up build. A shutdown event (Ctrl-C) cancels a running build and
//! stops the app. This is process restart, not in-process hot reload.

mod build;
mod command;
mod process;
mod watch;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_watch;

pub(crate) use build::BuildSpec;
pub(crate) use command::run;

use crate::error::CliError;
use build::{Built, build};
use process::App;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, UnboundedSender};

type Args = Vec<String>;
type Builds = UnboundedSender<PathBuf>;
type Code = Result<u8, CliError>;
type Events<'a> = &'a mut Receiver<Event>;
/// `Ok(None)` on shutdown, `Ok(Some(again))` after a build.
type Rebuilt = Result<Option<bool>, CliError>;

/// Default time the app gets to exit after SIGTERM.
pub(crate) const DEFAULT_GRACE: Duration = Duration::from_secs(5);

/// What drives the supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    /// Rebuild and, when it succeeds, restart (files changed, or the
    /// user asked).
    Changed,
    /// Stop everything and return (Ctrl-C).
    Shutdown,
}

/// The build/run state machine.
#[derive(Debug)]
pub(crate) struct Supervisor {
    spec: BuildSpec,
    /// Arguments for the app binary (`run ...`).
    args: Vec<String>,
    grace: Duration,
    app: Option<App>,
    /// Receives the executable of every successful build.
    built: Option<UnboundedSender<PathBuf>>,
}

/// How a build attempt ended.
enum Attempt {
    /// It finished; `again` when changes arrived meanwhile.
    Done(Result<Built, CliError>, bool),
    /// Shutdown was requested.
    Shutdown,
}

impl Supervisor {
    /// A supervisor for `spec` running the binary with `args`.
    pub(crate) fn new(spec: BuildSpec, args: Args, grace: Duration) -> Self {
        Self {
            spec,
            args,
            grace,
            app: None,
            built: None,
        }
    }

    /// Send the executable of every successful build to `built`.
    #[must_use]
    pub(crate) fn report_builds(mut self, built: Builds) -> Self {
        self.built = Some(built);
        self
    }

    /// Build and run, then rebuild on every [`Event::Changed`] until
    /// [`Event::Shutdown`] (or the event source closes).
    ///
    /// Errors:
    ///   Only failures to run cargo or the app at all; compile errors are
    ///   printed and wait for the next change.
    pub(crate) async fn run(mut self, events: Events<'_>) -> Code {
        let mut pending = true;
        loop {
            if pending {
                match self.rebuild(events).await {
                    Ok(Some(again)) => pending = again,
                    Ok(None) => break,
                    Err(err) => {
                        self.stop().await;
                        return Err(err);
                    }
                }
                continue;
            }
            tokio::select! {
                event = events.recv() => match event {
                    Some(Event::Changed) => pending = true,
                    Some(Event::Shutdown) | None => break,
                },
                status = exited(&mut self.app) => {
                    let shown = match status {
                        Ok(status) => status.to_string(),
                        Err(err) => err.to_string(),
                    };
                    log(&format!("the app exited ({shown}); waiting"));
                    self.app = None;
                }
            }
        }
        self.stop().await;
        Ok(0)
    }

    /// One build; on success swap the app. `Ok(None)` on shutdown,
    /// `Ok(Some(again))` otherwise.
    async fn rebuild(&mut self, events: Events<'_>) -> Rebuilt {
        log("building");
        let (outcome, again) = match attempt(&self.spec, events).await {
            Attempt::Done(outcome, again) => (outcome, again),
            Attempt::Shutdown => return Ok(None),
        };
        match outcome? {
            Built::Failed if self.app.is_some() => {
                log("build failed; the previous version keeps running");
            }
            Built::Failed => log("build failed; waiting for changes"),
            Built::Ready(exe) => {
                if let Some(built) = &self.built {
                    let _ = built.send(exe.clone());
                }
                self.swap(&exe).await?;
            }
        }
        Ok(Some(again))
    }

    /// Stop the old app (waiting until it is gone), then start `exe`.
    async fn swap(&mut self, exe: &std::path::Path) -> Result<(), CliError> {
        self.stop().await;
        let exe = runnable(exe)?;
        let app = App::start(&exe, &self.args, &self.spec.dir)?;
        log(&format!("started (pid {})", app.pid()));
        self.app = Some(app);
        Ok(())
    }

    async fn stop(&mut self) {
        if let Some(app) = self.app.take() {
            let pid = app.pid();
            let how = app.stop(self.grace).await;
            log(&format!("stopped pid {pid} ({how:?})"));
        }
    }
}

/// Run one build while watching for events: changes are coalesced into
/// one follow-up build, shutdown cancels the build (killing cargo).
async fn attempt(spec: &BuildSpec, events: &mut Receiver<Event>) -> Attempt {
    let building = build(spec);
    tokio::pin!(building);
    let mut again = false;
    loop {
        tokio::select! {
            outcome = &mut building => return Attempt::Done(outcome, again),
            event = events.recv() => match event {
                Some(Event::Changed) => again = true,
                Some(Event::Shutdown) | None => return Attempt::Shutdown,
            },
        }
    }
}

/// Wait for the app to exit; never resolves when there is none.
async fn exited(app: &mut Option<App>) -> std::io::Result<ExitStatus> {
    match app {
        Some(app) => app.exited().await,
        None => std::future::pending().await,
    }
}

/// The file to execute. Windows cannot replace a running executable, so
/// the next build would fail while the app runs; run a copy there.
fn runnable(exe: &std::path::Path) -> Result<PathBuf, CliError> {
    if !cfg!(windows) {
        return Ok(exe.to_path_buf());
    }
    let copy = exe.with_extension("dev.exe");
    let copied = std::fs::copy(exe, &copy);
    copied.map_err(|err| crate::fsio::io_error("copy", exe, &err))?;
    Ok(copy)
}

fn log(message: &str) {
    eprintln!("siderite dev: {message}");
}
