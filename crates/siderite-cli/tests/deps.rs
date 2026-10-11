//! `add` and `remove` reach Cargo for the selected package (stand-in
//! `cargo`, see `common`).

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

mod common;

use common::{fixture, scratch, siderite, stderr};
use std::fs;

#[test]
fn crate_edits_name_the_package_and_keep_exit_codes() {
    let root = scratch("deps");
    let (app, path) = fixture(&root.0);
    let log = root.0.join("args.txt");
    let log_path = log.display().to_string();
    let fake = [("FAKE_ARGS", log_path.as_str())];
    let args = ["add", "serde", "--features", "derive"];
    let out = siderite(&app, &path, &args, &fake);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let sent = fs::read_to_string(&log).unwrap();
    let sent: Vec<&str> = sent.lines().collect();
    assert_eq!(
        sent,
        ["add", "--package", "app", "serde", "--features", "derive"]
    );
    assert!(stderr(&out).contains("siderite: cargo add --package app serde"));
    let failing = [("FAKE_ARGS", log_path.as_str()), ("FAKE_CODE", "101")];
    let out = siderite(&app, &path, &["remove", "serde"], &failing);
    assert_eq!(out.status.code(), Some(101));
}

#[test]
fn presets_and_crates_do_not_mix() {
    let root = scratch("deps-mix");
    let (app, path) = fixture(&root.0);
    let args = ["add", "--preset", "postgres", "serde"];
    let out = siderite(&app, &path, &args, &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("cannot be combined"),
        "{}",
        stderr(&out)
    );
}
