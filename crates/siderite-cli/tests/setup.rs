//! `siderite setup --install` only runs rustup, and only what is needed.

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

mod common;

use common::{scratch, script, stderr, stdout};
use std::process::Command;

#[test]
fn install_runs_only_the_needed_rustup_steps() {
    let root = scratch("setup-install");
    let bin = root.0.join("bin");
    let home = root.0.join("home");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let log = root.0.join("rustup.log");
    let msrv = env!("CARGO_PKG_RUST_VERSION");
    script(&bin, "rustc", &format!("echo 'rustc {msrv}.0 (x)'"));
    script(&bin, "cc", "exit 0");
    let cargo = "case \"$1\" in\n--version) echo 'cargo 1';;\n\
                 fmt) echo 'rustfmt 1';;\nclippy) exit 1;;\nesac";
    script(&bin, "cargo", cargo);
    let rustup = format!(
        "if [ \"$1\" = --version ]; then echo 'rustup 1'; exit 0; fi\n\
         echo \"$*\" >> '{}'",
        log.display()
    );
    script(&bin, "rustup", &rustup);
    let out = Command::new(env!("CARGO_BIN_EXE_siderite"))
        .args(["setup", "--install"])
        .current_dir(&root.0)
        .env("PATH", &bin)
        .env("HOME", &home)
        .output()
        .unwrap();
    let ran = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(ran, "component add clippy\n", "{}", stderr(&out));
    assert!(stderr(&out).contains("running rustup component add clippy"));
    // The stand-in clippy stays missing, so the report still says so.
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(stdout(&out).contains("rustup component add clippy"));
    let touched: Vec<_> = std::fs::read_dir(&home).unwrap().collect();
    assert!(touched.is_empty(), "setup wrote into HOME: {touched:?}");
}
