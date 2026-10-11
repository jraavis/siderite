//! Shared helpers for the `siderite` binary tests: scratch directories,
//! an app package and a stand-in `cargo` on `PATH`.
//!
//! The stand-in prints noise on stdout (as an app factory might), logs its
//! arguments to stderr (and one per line to `$FAKE_ARGS`), writes
//! `$FAKE_REPORT` to the report file named by `SIDERITE_REPORT_PATH`, and
//! exits with `$FAKE_CODE`.

#![allow(dead_code, clippy::unwrap_used)]

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FAKE_CARGO: &str = r#"echo "noise from the app factory"
echo "cargo args: $*" >&2
if [ -n "$FAKE_ARGS" ]; then
  for arg in "$@"; do printf '%s\n' "$arg"; done > "$FAKE_ARGS"
fi
if [ -n "$SIDERITE_REPORT_PATH" ] && [ -n "$FAKE_REPORT" ]; then
  printf '%s\n' "$FAKE_REPORT" > "$SIDERITE_REPORT_PATH"
fi
exit "${FAKE_CODE:-0}""#;

/// A temp directory removed on drop.
pub struct Scratch(pub PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A fresh directory whose name contains a space.
pub fn scratch(name: &str) -> Scratch {
    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("siderite bin {id}-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

/// An executable shell script `dir/name` running `body`.
pub fn script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A plain app package and a `bin/cargo` stand-in under `root`. Returns
/// the package directory and a `PATH` that finds the stand-in first.
pub fn fixture(root: &Path) -> (PathBuf, String) {
    let app = root.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\
                    edition = \"2024\"\n\n[dependencies]\n\
                    siderite = \"0.1\"\n";
    fs::write(app.join("Cargo.toml"), manifest).unwrap();
    fs::write(app.join("src/main.rs"), "fn main() {}\n").unwrap();
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    script(&bin, "cargo", FAKE_CARGO);
    let path = std::env::var("PATH").unwrap_or_default();
    (app, format!("{}:{path}", bin.display()))
}

/// Extra environment for one run.
pub type Fake<'a> = &'a [(&'a str, &'a str)];

/// Run the `siderite` binary in `cwd` with `PATH` set to `path`.
pub fn siderite(cwd: &Path, path: &str, args: &[&str], fake: Fake) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_siderite"));
    command.args(args).current_dir(cwd).env("PATH", path);
    command.env_remove("SIDERITE_REPORT_PATH");
    command.env_remove("DATABASE_URL");
    for (key, value) in fake {
        command.env(key, value);
    }
    command.output().unwrap()
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Stdout must be exactly one JSON value and a newline.
pub fn one_value(out: &Output) -> Value {
    let text = stdout(out);
    assert!(text.ends_with('\n'), "{text:?}");
    assert_eq!(text.matches('\n').count(), 1, "{text:?}");
    serde_json::from_str(&text).unwrap()
}
