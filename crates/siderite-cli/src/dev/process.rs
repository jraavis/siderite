//! Owning child processes: start them in their own process group and
//! stop the whole group gracefully, then by force.
//!
//! On Unix each child leads a new process group, so everything it spawns
//! (rustc under cargo, workers under the app) is stopped with it, and a
//! terminal Ctrl-C reaches only `siderite`, which then stops the groups
//! itself. On other platforms only the direct child can be stopped, and
//! only by force.

use crate::error::CliError;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};

/// How long to wait for a killed group's members to disappear.
const GONE_BOUND: Duration = Duration::from_secs(2);

/// A process group led by a child we started (Unix), else just the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Group(u32);

impl Group {
    /// The group led by `pid`.
    pub(crate) const fn led_by(pid: u32) -> Self {
        Self(pid)
    }

    /// Ask every member to terminate (SIGTERM); `false` when the platform
    /// cannot.
    pub(crate) fn terminate(self) -> bool {
        self.signal(Signal::Term)
    }

    /// Kill every remaining member (SIGKILL).
    pub(crate) fn kill(self) {
        self.signal(Signal::Kill);
    }

    /// Wait (bounded) until no member is left, so ports and files they
    /// held are released.
    pub(crate) async fn gone(self) {
        let started = Instant::now();
        while self.exists() && started.elapsed() < GONE_BOUND {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(unix)]
    fn signal(self, signal: Signal) -> bool {
        use rustix::process::{Pid, Signal as Sig, kill_process_group};
        let sig = match signal {
            Signal::Term => Sig::TERM,
            Signal::Kill => Sig::KILL,
        };
        let pid = i32::try_from(self.0).ok().and_then(Pid::from_raw);
        pid.is_some_and(|pid| kill_process_group(pid, sig).is_ok())
    }

    #[cfg(not(unix))]
    fn signal(self, _signal: Signal) -> bool {
        false
    }

    /// Whether a live member is left. Zombies hold no ports or files and
    /// may wait for a slow init to reap them, so they do not count.
    #[cfg(target_os = "linux")]
    pub(crate) fn exists(self) -> bool {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return false;
        };
        entries.flatten().any(|entry| {
            let stat = entry.path().join("stat");
            let text = std::fs::read_to_string(stat).unwrap_or_default();
            live_member(&text, self.0)
        })
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    pub(crate) fn exists(self) -> bool {
        use rustix::process::{Pid, test_kill_process_group};
        let pid = i32::try_from(self.0).ok().and_then(Pid::from_raw);
        pid.is_some_and(|pid| test_kill_process_group(pid).is_ok())
    }

    #[cfg(not(unix))]
    pub(crate) fn exists(self) -> bool {
        false
    }
}

/// Whether a `/proc/PID/stat` line is a live process of group `pgid`:
/// after the parenthesised command come the state, ppid and pgrp.
#[cfg(any(target_os = "linux", test))]
fn live_member(stat: &str, pgid: u32) -> bool {
    let rest = stat.rsplit_once(')').map_or("", |(_, rest)| rest);
    let mut fields = rest.split_whitespace();
    let state = fields.next().unwrap_or("Z");
    let pgrp = fields.nth(1).and_then(|p| p.parse::<u32>().ok());
    state != "Z" && state != "X" && pgrp == Some(pgid)
}

#[derive(Debug, Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

/// Put `command` in a new process group (Unix) and tie the child's
/// lifetime to its handle.
pub(crate) fn own(command: &mut Command) {
    command.kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
}

type Started = Result<App, CliError>;

/// A running application process.
#[derive(Debug)]
pub(crate) struct App {
    child: Child,
    group: Group,
    pid: u32,
}

impl App {
    /// Start `exe args` in `dir`, inheriting stdout and stderr.
    ///
    /// Errors:
    ///   [`CliError::Io`] when the program cannot be started.
    pub(crate) fn start(exe: &Path, args: &[String], dir: &Path) -> Started {
        let mut command = Command::new(exe);
        command.args(args).current_dir(dir).stdin(Stdio::null());
        own(&mut command);
        let child = command
            .spawn()
            .map_err(|err| crate::fsio::io_error("start", exe, &err))?;
        let pid = child
            .id()
            .ok_or_else(|| CliError::Io("the app exited at once".into()))?;
        let group = Group::led_by(pid);
        Ok(Self { child, group, pid })
    }

    /// The process id.
    pub(crate) const fn pid(&self) -> u32 {
        self.pid
    }

    /// Wait until the process exits on its own.
    pub(crate) async fn exited(&mut self) -> std::io::Result<ExitStatus> {
        self.child.wait().await
    }

    /// Stop the process: SIGTERM its group, wait up to `grace`, then kill
    /// it. Returns once the process is reaped and the rest of its group
    /// is gone (bounded), so its ports are free.
    pub(crate) async fn stop(mut self, grace: Duration) -> Stopped {
        let how = if let Ok(Some(_)) = self.child.try_wait() {
            Stopped::AlreadyExited
        } else if self.group.terminate()
            && tokio::time::timeout(grace, self.child.wait()).await.is_ok()
        {
            Stopped::Graceful
        } else {
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
            Stopped::Killed
        };
        // Members the app started must not outlive it.
        self.group.kill();
        self.group.gone().await;
        how
    }
}

/// How [`App::stop`] ended the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stopped {
    /// It had already exited.
    AlreadyExited,
    /// It exited within the grace period after SIGTERM.
    Graceful,
    /// It was killed after the grace period (or could not be asked).
    Killed,
}

/// Kills a group when dropped before [`Self::disarm`] (a cancelled
/// build must not leave rustc running).
#[derive(Debug)]
pub(crate) struct KillOnDrop(Option<Group>);

impl KillOnDrop {
    /// Guard `group`.
    pub(crate) const fn new(group: Group) -> Self {
        Self(Some(group))
    }

    /// The work finished; do nothing on drop.
    pub(crate) fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(group) = self.0.take() {
            group.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::live_member;

    #[test]
    fn stat_lines_identify_live_members() {
        let line = "42 (my (odd) app) S 1 40 40 0 -1";
        assert!(live_member(line, 40));
        assert!(!live_member(line, 41));
        assert!(!live_member("43 (sleep) Z 1 40 40", 40));
        assert!(!live_member("", 40));
    }
}
