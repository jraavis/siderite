//! `build`, `test`, `fmt`, `lint` and `clean` forward to Cargo with their
//! arguments and exit status intact (stand-in `cargo`, see `common`).

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

mod common;

use common::{fixture, scratch, script, siderite, stderr};
use std::fs;
use std::path::Path;

type Lines = Vec<String>;

/// The arguments the stand-in `cargo` received, one per entry.
fn forwarded(root: &Path, app: &Path, path: &str, args: &[&str]) -> Lines {
    let log = root.join("args.txt");
    let log_path = log.display().to_string();
    let fake = [("FAKE_ARGS", log_path.as_str())];
    let out = siderite(app, path, args, &fake);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = fs::read_to_string(&log).unwrap();
    text.lines().map(str::to_owned).collect()
}

/// Stand-ins for the rustup components, so `cargo fmt`/`clippy` exist.
fn components(root: &Path) {
    let bin = root.join("bin");
    script(&bin, "cargo-fmt", "exit 0");
    script(&bin, "cargo-clippy", "exit 0");
}

#[test]
fn commands_map_to_cargo_with_argument_boundaries() {
    let root = scratch("passthrough");
    let (app, path) = fixture(&root.0);
    components(&root.0);
    let cases: [(&[&str], &[&str]); 5] = [
        (
            &["lint", "--", "-D", "warnings", "a b"],
            &["clippy", "--", "-D", "warnings", "a b"],
        ),
        (&["fmt", "--check"], &["fmt", "--check"]),
        (&["clean"], &["clean"]),
        (&["build", "--release"], &["build", "--release"]),
        (
            &["test", "--", "--nocapture"],
            &["test", "--", "--nocapture"],
        ),
    ];
    for (args, want) in cases {
        assert_eq!(forwarded(&root.0, &app, &path, args), want, "{args:?}");
    }
    // `lint` is Clippy; the framework's `check` still runs the app.
    let check = forwarded(&root.0, &app, &path, &["check"]);
    assert_eq!(check, ["run", "--", "check"]);
}

#[test]
fn child_exit_status_is_forwarded() {
    let root = scratch("passthrough-exit");
    let (app, path) = fixture(&root.0);
    components(&root.0);
    for args in [["lint"], ["fmt"], ["clean"]] {
        let out = siderite(&app, &path, &args, &[("FAKE_CODE", "7")]);
        assert_eq!(out.status.code(), Some(7), "{args:?}");
    }
}

#[test]
fn missing_components_and_bad_flags_are_clear() {
    let root = scratch("passthrough-missing");
    let (app, _) = fixture(&root.0);
    // Only the stand-in directory: no cargo-fmt or cargo-clippy anywhere.
    let path = root.0.join("bin").display().to_string();
    for (cmd, component) in [("fmt", "rustfmt"), ("lint", "clippy")] {
        let out = siderite(&app, &path, &[cmd], &[]);
        assert_eq!(out.status.code(), Some(1), "{cmd}");
        let text = stderr(&out);
        let hint = format!("rustup component add {component}");
        assert!(text.contains(&hint), "{text}");
    }
    components(&root.0);
    let out = siderite(&app, &path, &["--bin", "app", "fmt"], &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--bin does not apply"));
}
