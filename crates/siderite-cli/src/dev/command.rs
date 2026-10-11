//! `siderite dev [--grace-ms N] [-- APP ARGS]`.

use super::watch::{Inputs, Layout, Watch};
use super::{BuildSpec, DEFAULT_GRACE, Event, Supervisor};
use crate::error::CliError;
use crate::target::{self, ProjectArgs};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;

type Code = Result<u8, CliError>;

const USAGE: &str = "usage: dev [--grace-ms N] [-- ARGS FOR `run`]";

/// Parsed `dev` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    /// Time the app gets to exit after SIGTERM.
    pub grace: Duration,
    /// Arguments for the app binary: `run` and what follows `--`.
    pub app_args: Vec<String>,
}

/// Parse the arguments from `dev` on.
///
/// Errors:
///   A usage error for unknown flags or a bad grace period.
pub(crate) fn parse(raw: &[String]) -> Result<Options, CliError> {
    let mut grace = DEFAULT_GRACE;
    let mut app_args = vec!["run".to_owned()];
    let mut args = raw.iter().skip_while(|a| *a != "dev").skip(1);
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (arg.as_str(), None),
        };
        match flag {
            "--" => {
                app_args.extend(args.by_ref().cloned());
            }
            "--grace-ms" => {
                let value = inline.or_else(|| args.next().map(String::as_str));
                grace = millis(value)?;
            }
            _ => {
                let msg = format!("unexpected argument `{arg}`; {USAGE}");
                return Err(CliError::usage(msg));
            }
        }
    }
    Ok(Options { grace, app_args })
}

fn millis(value: Option<&str>) -> Result<Duration, CliError> {
    match value.map(str::parse::<u64>) {
        Some(Ok(ms)) => Ok(Duration::from_millis(ms)),
        _ => Err(CliError::usage("--grace-ms needs a whole number")),
    }
}

/// Run `dev` for the selected application until Ctrl-C.
///
/// Errors:
///   Usage errors, a missing application, or failures to run cargo or
///   the app at all.
pub(crate) async fn run(raw: &[String], cwd: &Path, p: &ProjectArgs) -> Code {
    let options = parse(raw)?;
    let app = target::locate("cargo", cwd, p, true)?.ok_or_else(|| {
        CliError::usage(
            "`dev` needs an application. cd into a project created with \
             `siderite new`",
        )
    })?;
    let layout = layout(&app.dir);
    let spec = BuildSpec {
        cargo: PathBuf::from("cargo"),
        dir: app.dir,
        bin: app.bin.or(app.default_run),
        release: false,
    };
    let (events, mut received) = mpsc::channel(64);
    let (built, builds) = mpsc::unbounded_channel();
    let watching = match Watch::start() {
        Ok(watch) => Some(tokio::spawn(watch_loop(
            watch,
            layout,
            builds,
            events.clone(),
        ))),
        Err(err) => {
            eprintln!("siderite dev: cannot watch files ({err}); use r+Enter");
            None
        }
    };
    let shutdown = events.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutdown.send(Event::Shutdown).await;
        }
    });
    tokio::spawn(restart_on_enter(events.clone()));
    let Options { grace, app_args } = options;
    let supervisor = Supervisor::new(spec, app_args, grace);
    let supervisor = supervisor.report_builds(built);
    let code = supervisor.run(&mut received).await;
    if let Some(watching) = watching {
        watching.abort();
    }
    drop(events);
    code
}

/// Where the package, workspace, target and migrations directories are.
/// `cargo metadata` is asked once, when `dev` starts.
fn layout(package: &Path) -> Layout {
    let manifest = package.join("Cargo.toml");
    let found = crate::workspace::load("cargo", package, Some(&manifest)).ok();
    let root = found.as_ref().and_then(|w| w.root.clone());
    let target = found.and_then(|w| w.target_dir);
    Layout {
        package: package.to_path_buf(),
        root: root.unwrap_or_else(|| package.to_path_buf()),
        target: target.unwrap_or_else(|| package.join("target")),
        migrations: package.join("migrations"),
    }
}

/// Forward debounced changes as rebuilds; after each successful build,
/// watch the inputs listed in the new binary's dep-info.
pub(super) async fn watch_loop(
    mut watch: Watch,
    layout: Layout,
    mut builds: mpsc::UnboundedReceiver<PathBuf>,
    events: mpsc::Sender<Event>,
) {
    let mut inputs = Inputs::initial(&layout);
    watch.set(&inputs);
    loop {
        tokio::select! {
            changed = watch.changed(&inputs) => {
                let Some(event) = changed else { return };
                if events.send(event).await.is_err() {
                    return;
                }
            }
            exe = builds.recv() => {
                let Some(exe) = exe else { return };
                inputs = Inputs::built(&layout, &exe);
                watch.set(&inputs);
            }
        }
    }
}

/// `r` + Enter on stdin rebuilds and restarts. The app never reads
/// stdin (it is not attached), so the terminal belongs to `siderite`.
async fn restart_on_enter(events: mpsc::Sender<Event>) {
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if is_restart(&line) && events.send(Event::Changed).await.is_err() {
            return;
        }
    }
}

fn is_restart(line: &str) -> bool {
    matches!(line.trim(), "r" | "rs" | "restart")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn restart_commands() {
        for line in ["r\n", " rs ", "restart"] {
            assert!(is_restart(line), "{line:?}");
        }
        assert!(!is_restart("") && !is_restart("q"));
    }

    #[test]
    fn parses_grace_and_app_arguments() {
        let parsed = parse(&args(&["dev"])).unwrap();
        assert_eq!(parsed.app_args, ["run"]);
        assert_eq!(parsed.grace, DEFAULT_GRACE);
        let list = ["dev", "--grace-ms=250", "--", "--addr", "0.0.0.0:1"];
        let parsed = parse(&args(&list)).unwrap();
        assert_eq!(parsed.grace, Duration::from_millis(250));
        assert_eq!(parsed.app_args, ["run", "--addr", "0.0.0.0:1"]);
        for bad in [["dev", "--grace-ms", "x"], ["dev", "--watch", "y"]] {
            assert_eq!(parse(&args(&bad)).unwrap_err().exit_code(), 2);
        }
    }
}
