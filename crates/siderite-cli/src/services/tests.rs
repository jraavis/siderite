//! Local services against a scripted runtime, and once against a real
//! one (`--ignored`, needs Docker or Podman).

#![allow(clippy::unwrap_used)]

use super::*;
use crate::fixtures::{ScratchDir, assert_schema_valid};

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

fn options(words: &[&str]) -> Result<Options, CliError> {
    parse(&args(words))
}

#[test]
fn arguments() {
    let up = options(&["services", "up", "postgres", "--port", "5999"]);
    let up = up.unwrap();
    assert_eq!((up.action, up.wanted.port), (Action::Up, Some(5999)));
    let up = options(&["services", "up", "--timeout", "5", "--json"]);
    assert_eq!(up.unwrap().timeout, Duration::from_secs(5));
    let destroy = options(&["services", "destroy", "postgres", "--yes"]);
    assert_eq!(destroy.unwrap().action, Action::Destroy);
    for bad in [
        &["services"][..],
        &["services", "restart"],
        &["services", "up", "redis"],
        &["services", "up", "--port", "x"],
        &["services", "up", "--port"],
        &["services", "down", "--port", "1"],
        &["services", "env", "--json"],
        &["services", "destroy", "postgres"],
        &["services", "destroy", "--yes"],
        &["services", "status", "--yes"],
    ] {
        assert!(options(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_missing_runtime_is_reported_and_writes_nothing() {
    let dir = ScratchDir::new("services");
    let empty = std::ffi::OsString::new();
    let none = Runtime::find(None, &empty);
    assert_eq!(none.as_ref().unwrap_err().code, "services.E001");
    let named = Runtime::find(Some("/opt/podman".as_ref()), &empty);
    assert_eq!(named.unwrap().program, Path::new("/opt/podman"));
    let up = options(&["services", "up"]).unwrap();
    let report = execute(&up, none, dir.path()).unwrap();
    assert_eq!(report.exit_code(), 1);
    assert_eq!(report.diagnostics[0].code(), "services.E001");
    assert!(!dir.path().join(state::DIR).exists());
    let text = render(&report, Action::Up);
    assert!(text.contains("hint: install Docker or Podman"), "{text}");
}

/// A runtime script that keeps container and volume state as files in
/// `dir` and logs every command line.
#[cfg(unix)]
fn fake_runtime(dir: &Path) -> Runtime {
    use std::os::unix::fs::PermissionsExt;
    let s = dir.display();
    let script = format!(
        r#"#!/bin/sh
S="{s}"
echo "$*" >> "$S/log"
[ -n "$POSTGRES_PASSWORD" ] && echo "env:password" >> "$S/log"
missing() {{ echo "Error: No such object: $1" >&2; exit 1; }}
case "$1 $2" in
"container inspect") [ -f "$S/container" ] || missing c
    cat "$S/container"; exit 0 ;;
"volume inspect") [ -f "$S/volume" ] || missing v; exit 0 ;;
"volume rm") rm "$S/volume"; exit 0 ;;
esac
case "$1" in
run) echo running > "$S/container"; touch "$S/volume"
    [ -f "$S/crashes" ] && echo exited > "$S/container" ;;
start) echo running > "$S/container" ;;
stop) echo exited > "$S/container" ;;
rm) rm -f "$S/container" ;;
exec) [ -f "$S/notready" ] && exit 1 ;;
esac
exit 0
"#
    );
    let program = dir.join("runtime");
    std::fs::write(&program, script).unwrap();
    let mode = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(&program, mode).unwrap();
    Runtime { program }
}

#[cfg(unix)]
fn step(runtime: &Runtime, dir: &Path, words: &[&str]) -> Report {
    let options = options(words).unwrap();
    let report = execute(&options, Ok(runtime.clone()), dir).unwrap();
    let json = serde_json::to_value(&report).unwrap();
    assert_schema_valid(&json);
    report
}

#[cfg(unix)]
#[test]
fn up_down_and_destroy_keep_data_until_asked() {
    use std::os::unix::fs::PermissionsExt;
    let fake = ScratchDir::new("services-runtime");
    let project = ScratchDir::new("services-blog");
    let (runtime, dir) = (fake_runtime(fake.path()), project.path());
    let up = step(&runtime, dir, &["services", "up", "--json"]);
    assert!(up.ok, "{:?}", up.diagnostics);
    let service = &up.data.services[0];
    assert_eq!(
        (service.status, service.data_volume),
        (Status::Running, true)
    );
    assert!(service.address.starts_with("127.0.0.1:"));
    let recorded = state::load(dir).unwrap().unwrap();
    assert!(!service.url.contains(&recorded.password), "{}", service.url);
    let local = dir.join(state::DIR);
    let mode = std::fs::metadata(&local).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    let ignore = std::fs::read_to_string(local.join(".gitignore")).unwrap();
    assert_eq!(ignore, "*\n");

    let log = std::fs::read_to_string(fake.path().join("log")).unwrap();
    assert!(!log.contains(&recorded.password), "{log}");
    assert!(log.contains("env:password"), "{log}");
    let publish = format!("--publish 127.0.0.1:{}:5432", recorded.port);
    assert!(log.contains(&publish), "{log}");

    let down = step(&runtime, dir, &["services", "down"]);
    let service = &down.data.services[0];
    assert_eq!(
        (service.status, service.data_volume),
        (Status::Absent, true)
    );
    let again = step(&runtime, dir, &["services", "up"]);
    assert_eq!(again.data.services[0].volume, service.volume);
    assert_eq!(state::load(dir).unwrap().unwrap(), recorded);

    let destroy = ["services", "destroy", "postgres", "--yes"];
    let gone = step(&runtime, dir, &destroy);
    assert!(gone.ok && gone.data.services.is_empty());
    assert!(!fake.path().join("volume").exists());
    assert!(state::load(dir).unwrap().is_none());
    let status = step(&runtime, dir, &["services", "status"]);
    let text = render(&status, Action::Status);
    assert!(text.contains("no services here"), "{text}");
}

#[cfg(unix)]
#[test]
fn a_database_that_never_gets_ready_times_out() {
    let fake = ScratchDir::new("services-runtime");
    let project = ScratchDir::new("services-slow");
    let runtime = fake_runtime(fake.path());
    std::fs::write(fake.path().join("notready"), "").unwrap();
    let words = ["services", "up", "--timeout", "1"];
    let report = step(&runtime, project.path(), &words);
    assert_eq!(report.diagnostics[0].code(), "services.E003");
    let crashed = ScratchDir::new("services-crash");
    std::fs::remove_file(fake.path().join("container")).unwrap();
    std::fs::write(fake.path().join("crashes"), "").unwrap();
    let words = ["services", "up", "--timeout", "30"];
    let report = step(&runtime, crashed.path(), &words);
    let diag = &report.diagnostics[0];
    assert_eq!(diag.code(), "services.E002");
    assert!(diag.message().contains("stopped while starting"));
    assert_eq!(report.data.services.len(), 0);
}

/// The real runtime: data written before `down` is there after `up`,
/// and `destroy` removes the volume.
#[test]
#[ignore = "needs Docker or Podman and the postgres image"]
fn live_postgres_keeps_data_across_down_and_up() {
    let project = ScratchDir::new("services-live");
    let dir = project.path();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let chosen = std::env::var_os(RUNTIME_ENV);
    let runtime = Runtime::find(chosen.as_deref(), &path).unwrap();
    let run = |words: &[&str]| {
        let options = options(words).unwrap();
        let report = execute(&options, Ok(runtime.clone()), dir).unwrap();
        assert!(report.ok, "{words:?}: {:?}", report.diagnostics);
        report
    };
    let up = run(&["services", "up"]);
    let container = up.data.services[0].container.clone();
    let recorded = state::load(dir).unwrap().unwrap();
    let psql = |sql: &str| {
        let args = [
            "exec",
            &container,
            "psql",
            "-qtA",
            "-U",
            &recorded.user,
            "-d",
            &recorded.database,
            "-c",
            sql,
        ];
        runtime.run(&args, &[])
    };
    psql("CREATE TABLE kept (n int); INSERT INTO kept VALUES (7)").unwrap();
    run(&["services", "down"]);
    run(&["services", "up"]);
    assert_eq!(psql("SELECT n FROM kept").unwrap(), "7");
    let volume = up.data.services[0].volume.clone();
    run(&["services", "destroy", "postgres", "--yes"]);
    assert!(!runtime.volume(&volume).unwrap());
}
