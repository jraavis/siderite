//! Supervisor behaviour with a stand-in `cargo` and app (Unix only).
//!
//! The stand-in build appends to `builds`, fails while `fail` exists and
//! otherwise reports the app script as the built binary. The app records
//! its pid and its background child's pid, and either exits on SIGTERM
//! (recording `graceful`) or ignores it.

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use super::*;
use crate::fixtures::{ScratchDir, block_on};
use crate::process;
use std::path::Path;
use tokio::sync::mpsc;

fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mode = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(path, mode).unwrap();
}

/// A stand-in project in `dir`; `build_secs` slows each build down.
fn project(dir: &Path, graceful: bool, build_secs: &str) -> BuildSpec {
    let d = dir.display();
    script(
        &dir.join("cargo"),
        &format!(
            "echo build >> '{d}/builds'\n\
             echo $$ >> '{d}/build-pids'\n\
             sleep {build_secs}\n\
             if [ -e '{d}/fail' ]; then\n\
             printf '%s\\n' '{{\"reason\":\"compiler-message\",\
             \"message\":{{\"rendered\":\"error: broken\\n\"}}}}'\n\
             exit 101\n\
             fi\n\
             printf '{{\"reason\":\"compiler-artifact\",\"target\":\
             {{\"kind\":[\"bin\"],\"name\":\"app\"}},\
             \"executable\":\"%s\"}}\\n' '{d}/app'"
        ),
    );
    let trap = if graceful {
        format!("trap 'echo graceful >> \"{d}/events\"; exit 0' TERM")
    } else {
        "trap '' TERM".to_owned()
    };
    script(
        &dir.join("app"),
        &format!(
            "echo $$ >> '{d}/pids'\n\
             {trap}\n\
             sleep 1000 &\n\
             echo $! >> '{d}/children'\n\
             while true; do sleep 0.05; done"
        ),
    );
    BuildSpec {
        cargo: dir.join("cargo"),
        dir: dir.to_path_buf(),
        bin: None,
        release: false,
    }
}

fn lines(dir: &Path, name: &str) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    text.lines().map(str::to_owned).collect()
}

fn pids(dir: &Path, name: &str) -> Vec<u32> {
    lines(dir, name)
        .iter()
        .map(|l| l.parse().unwrap())
        .collect()
}

/// Whether `pid` is a live (not zombie) process.
fn alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
        let Ok(stat) = stat else { return false };
        let state = stat.rsplit(')').next().unwrap_or("").trim_start();
        return !state.starts_with('Z');
    }
    let pid = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw);
    pid.is_some_and(|p| rustix::process::test_kill_process(p).is_ok())
}

/// Poll `ready` for up to ten seconds.
async fn until(what: &str, ready: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while !ready() {
        assert!(started.elapsed().as_secs() < 10, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

type Handle = tokio::task::JoinHandle<Result<u8, CliError>>;

fn spawn(spec: BuildSpec, grace: Duration) -> (mpsc::Sender<Event>, Handle) {
    let (tx, mut rx) = mpsc::channel(16);
    let supervisor = Supervisor::new(spec, vec!["run".into()], grace);
    let handle = tokio::spawn(async move { supervisor.run(&mut rx).await });
    (tx, handle)
}

#[test]
fn a_failed_build_keeps_the_healthy_app_and_a_good_one_replaces_it() {
    let scratch = ScratchDir::new("dev-swap");
    let dir = scratch.path().to_path_buf();
    let spec = project(&dir, true, "0");
    block_on(async {
        let (tx, handle) = spawn(spec, DEFAULT_GRACE);
        until("first start", || pids(&dir, "pids").len() == 1).await;
        let first = pids(&dir, "pids")[0];
        std::fs::write(dir.join("fail"), "").unwrap();
        tx.send(Event::Changed).await.unwrap();
        until("failed build", || lines(&dir, "builds").len() == 2).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(alive(first), "the healthy app must keep running");
        assert_eq!(pids(&dir, "pids").len(), 1);
        std::fs::remove_file(dir.join("fail")).unwrap();
        tx.send(Event::Changed).await.unwrap();
        until("restart", || pids(&dir, "pids").len() == 2).await;
        assert!(!alive(first));
        assert_eq!(lines(&dir, "events"), ["graceful"]);
        let second = pids(&dir, "pids")[1];
        assert!(alive(second));
        tx.send(Event::Shutdown).await.unwrap();
        assert_eq!(handle.await.unwrap().unwrap(), 0);
        assert!(!alive(second));
        for child in pids(&dir, "children") {
            until("background child gone", || !alive(child)).await;
        }
    });
}

#[test]
fn an_app_ignoring_sigterm_is_killed_after_the_grace_period() {
    let scratch = ScratchDir::new("dev-stubborn");
    let dir = scratch.path().to_path_buf();
    let spec = project(&dir, false, "0");
    block_on(async {
        let (tx, handle) = spawn(spec, Duration::from_millis(300));
        until("first start", || pids(&dir, "pids").len() == 1).await;
        let first = pids(&dir, "pids")[0];
        let started = std::time::Instant::now();
        tx.send(Event::Changed).await.unwrap();
        until("restart", || pids(&dir, "pids").len() == 2).await;
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert!(!alive(first));
        assert!(lines(&dir, "events").is_empty());
        drop(tx);
        assert_eq!(handle.await.unwrap().unwrap(), 0);
        let all = pids(&dir, "pids").into_iter().chain(pids(&dir, "children"));
        for pid in all {
            assert!(!alive(pid), "{pid} survived");
        }
    });
}

#[test]
fn changes_during_a_build_coalesce_into_one_more_build() {
    let scratch = ScratchDir::new("dev-coalesce");
    let dir = scratch.path().to_path_buf();
    let spec = project(&dir, true, "0.5");
    block_on(async {
        let (tx, handle) = spawn(spec, DEFAULT_GRACE);
        until("first build", || lines(&dir, "builds").len() == 1).await;
        for _ in 0..5 {
            tx.send(Event::Changed).await.unwrap();
        }
        until("second start", || pids(&dir, "pids").len() == 2).await;
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(lines(&dir, "builds").len(), 2);
        tx.send(Event::Shutdown).await.unwrap();
        handle.await.unwrap().unwrap();
    });
}

#[test]
fn shutdown_cancels_a_running_build() {
    let scratch = ScratchDir::new("dev-cancel");
    let dir = scratch.path().to_path_buf();
    let spec = project(&dir, true, "30");
    block_on(async {
        let (tx, handle) = spawn(spec, DEFAULT_GRACE);
        until("build started", || lines(&dir, "builds").len() == 1).await;
        let started = std::time::Instant::now();
        tx.send(Event::Shutdown).await.unwrap();
        assert_eq!(handle.await.unwrap().unwrap(), 0);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(pids(&dir, "pids").is_empty());
        // The stand-in build's `sleep` died with its process group.
        let leader = pids(&dir, "build-pids")[0];
        let group = process::Group::led_by(leader);
        until("build group gone", || !group.exists()).await;
    });
}

#[test]
fn source_edits_restart_and_runtime_files_do_not() {
    use super::watch::{Layout, Watch};
    let scratch = ScratchDir::new("dev-loop");
    let dir = scratch.path().to_path_buf();
    let spec = project(&dir, true, "0");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
    let main = dir.join("src/main.rs");
    let deps = format!("{}: {}\n", dir.join("app").display(), main.display());
    std::fs::write(dir.join("app.d"), deps).unwrap();
    let layout = Layout {
        package: dir.clone(),
        root: dir.clone(),
        target: dir.join("target"),
        migrations: dir.join("migrations"),
    };
    block_on(async {
        let (tx, mut rx) = mpsc::channel(16);
        let (built, builds) = mpsc::unbounded_channel();
        let watch = Watch::start().unwrap();
        let watched = command::watch_loop(watch, layout, builds, tx.clone());
        let watching = tokio::spawn(watched);
        let args = vec!["run".to_owned()];
        let supervisor = Supervisor::new(spec, args, DEFAULT_GRACE);
        let supervisor = supervisor.report_builds(built);
        let running = async move { supervisor.run(&mut rx).await };
        let handle = tokio::spawn(running);
        until("first start", || pids(&dir, "pids").len() == 1).await;
        // Let the watcher switch to the dep-info inputs.
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(&main, "fn main() { }\n").unwrap();
        until("restart on edit", || pids(&dir, "pids").len() == 2).await;
        for i in 0..10 {
            std::fs::write(dir.join("app.db"), format!("{i}")).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(pids(&dir, "pids").len(), 2, "runtime files restarted");
        assert_eq!(lines(&dir, "builds").len(), 2);
        tx.send(Event::Shutdown).await.unwrap();
        handle.await.unwrap().unwrap();
        watching.abort();
    });
}
