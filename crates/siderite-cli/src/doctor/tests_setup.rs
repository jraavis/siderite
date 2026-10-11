//! `setup` against stand-in toolchains (Unix).

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use super::setup::{diagnose, parse, steps_for};
use super::*;
use crate::fixtures::ScratchDir;
use std::path::Path;

fn script(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mode = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(&path, mode).unwrap();
}

/// A stand-in toolchain; `clippy` and `cc` are optional.
fn tools(dir: &Path, clippy: bool, cc: bool) -> OsString {
    let msrv = env!("CARGO_PKG_RUST_VERSION");
    script(dir, "rustc", &format!("echo 'rustc {msrv}.0 (x)'"));
    script(dir, "rustup", "echo 'rustup 1.29.0'");
    let clippy = if clippy {
        "echo 'clippy 0.1'"
    } else {
        "exit 1"
    };
    let cargo = format!(
        "case \"$1\" in\n--version) echo 'cargo 1.99.0';;\n\
         fmt) echo 'rustfmt 1.8';;\nclippy) {clippy};;\nesac"
    );
    script(dir, "cargo", &cargo);
    if cc {
        script(dir, "cc", "exit 0");
    }
    dir.as_os_str().to_owned()
}

fn codes(report: &Report) -> Vec<&str> {
    report.diagnostics.iter().map(Diagnostic::code).collect()
}

#[test]
fn a_complete_setup_passes() {
    let bin = ScratchDir::new("setup-ok");
    let path = tools(bin.path(), true, true);
    let report = diagnose(&path, bin.path());
    assert!(report.ok, "{}", render(&report));
    assert_eq!(report.command, "setup");
    let checks = report.data.checks.iter();
    let names: Vec<_> = checks.map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "rustup",
            "toolchain.rustc",
            "toolchain.cargo",
            "toolchain.rustfmt",
            "toolchain.clippy",
            "linker"
        ]
    );
}

#[test]
fn missing_pieces_name_the_exact_fix() {
    let bin = ScratchDir::new("setup-missing");
    let path = tools(bin.path(), false, false);
    let report = diagnose(&path, bin.path());
    assert_eq!(codes(&report), ["doctor.W002", "setup.E001"]);
    let text = render(&report);
    assert!(text.contains("rustup component add clippy"), "{text}");
    let linker_hint = report.diagnostics[1].hint().unwrap();
    let mac = cfg!(target_os = "macos");
    assert!(linker_hint.contains("build-essential") || mac);
    assert_eq!(steps_for(&path, bin.path()), ["component add clippy"]);
    let empty = ScratchDir::new("setup-empty");
    let none = empty.path().as_os_str().to_owned();
    let steps = steps_for(&none, empty.path());
    assert_eq!(steps[..2], ["toolchain install stable", "default stable"]);
}

#[test]
fn options_reject_json_with_install() {
    let args = |l: &[&str]| -> Vec<String> {
        let owned = l.iter().map(|a| (*a).to_owned());
        owned.collect()
    };
    assert!(parse(&args(&["setup", "--install"])).unwrap().install);
    assert!(parse(&args(&["setup", "--json"])).unwrap().json);
    let both = parse(&args(&["setup", "--json", "--install"]));
    assert_eq!(both.unwrap_err().exit_code(), 2);
    assert!(parse(&args(&["setup", "--yes"])).is_err());
}
