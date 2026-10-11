//! The CI workflow: toolchain pinning, CLI installation, services and
//! replacement rules.

#![allow(clippy::unwrap_used)]

use super::ci::*;
use crate::fixtures::ScratchDir;
use crate::workspace::Package;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn package(cli: Value) -> Package {
    Package {
        name: "shop".into(),
        manifest_path: PathBuf::from("/repo/shop/Cargo.toml"),
        uses_siderite: true,
        bins: vec!["shop".into()],
        default_run: None,
        dependencies: vec!["siderite-cli".into()],
        rust_version: None,
        version: "0.1.0".into(),
        siderite: Value::Null,
        specs: vec![cli],
    }
}

const GIT_LOCK: &str = "\
[[package]]
name = \"siderite-cli\"
version = \"0.1.0\"
source = \"git+https://github.com/o/siderite?branch=main#abc123\"
";

#[test]
fn the_cli_is_installed_as_the_lock_pins_it() {
    let repo = Path::new("/repo");
    let git = package(json!({ "name": "siderite-cli",
        "features": ["postgres"] }));
    let command = install(&git, GIT_LOCK, repo).unwrap();
    assert_eq!(
        command,
        "cargo install --locked --git https://github.com/o/siderite \
         --rev abc123 siderite-cli --features postgres"
    );
    assert_eq!(databases(&git), ["postgres"]);
    let registry = GIT_LOCK.replace(
        "git+https://github.com/o/siderite?branch=main#abc123",
        "registry+https://github.com/rust-lang/crates.io-index",
    );
    let plain = package(json!({ "name": "siderite-cli" }));
    let command = install(&plain, &registry, repo).unwrap();
    assert_eq!(
        command,
        "cargo install --locked siderite-cli --version =0.1.0"
    );
    assert!(databases(&plain).is_empty());
    let inside = package(json!({ "name": "siderite-cli",
        "path": "/repo/crates/siderite-cli" }));
    let command = install(&inside, "", repo).unwrap();
    assert!(command.ends_with("/crates/siderite-cli"), "{command}");
    assert!(command.contains("${{ github.workspace }}"), "{command}");
    let outside = package(json!({ "name": "siderite-cli",
        "path": "/elsewhere/siderite-cli" }));
    assert!(install(&outside, "", repo).is_err());
    assert!(install(&plain, "", repo).is_err(), "not locked");
}

#[test]
fn the_toolchain_is_always_pinned() {
    let dir = ScratchDir::new("ci-toolchain");
    let current = "rustc 1.99.0 (abc 2026-01-01)";
    let pinned = toolchain(dir.path(), None, current);
    assert_eq!(pinned, ("1.99.0".to_owned(), "current rustc"));
    let msrv = toolchain(dir.path(), Some("1.95"), current);
    assert_eq!(msrv, ("1.95".to_owned(), "rust-version"));
    let file = dir.path().join("rust-toolchain.toml");
    std::fs::write(&file, "[toolchain]\nchannel = \"1.97.1\"\n").unwrap();
    let from_file = toolchain(dir.path(), Some("1.95"), current);
    assert_eq!(from_file, ("1.97.1".to_owned(), "toolchain file"));
    std::fs::remove_file(file).unwrap();
    std::fs::write(dir.path().join("rust-toolchain"), "1.96.0\n").unwrap();
    assert_eq!(toolchain(dir.path(), None, current).0, "1.96.0");
}

fn inputs(databases: Vec<&'static str>, working_dir: &str) -> Inputs {
    Inputs {
        working_dir: working_dir.to_owned(),
        toolchain: ("1.99.0".to_owned(), "rust-version"),
        install: "cargo install --locked siderite-cli".to_owned(),
        databases,
    }
}

#[test]
fn the_workflow_verifies_and_runs_live_tests_only_when_asked() {
    let text = render(&inputs(vec!["postgres", "mysql"], "apps/shop"));
    assert!(text.starts_with(MARKER));
    for want in [
        "siderite verify --json | tee verify.json",
        "siderite verify --live --json | tee verify-live.json",
        "github.event_name == 'workflow_dispatch'",
        "toolchain: \"1.99.0\"",
        "working-directory: apps/shop",
        "POSTGRES_HOST_AUTH_METHOD: trust",
        "MYSQL_ALLOW_EMPTY_PASSWORD",
        "DATABASE_URL: postgres://postgres@localhost:5432/app",
        "uses: actions/upload-artifact@v4",
        "if: always()",
    ] {
        assert!(text.contains(want), "{want}\n{text}");
    }
    // No secrets: no `secrets.` references and no password in a URL.
    assert!(!text.contains("secrets."), "{text}");
    for url in text.split("://").skip(1) {
        let authority = url.split(['/', ' ', '\n']).next().unwrap();
        let user = authority.split_once('@').map_or("", |(u, _)| u);
        assert!(!user.contains(':'), "{authority}");
    }
    // Steps stay indented under `steps:` (no lost leading spaces).
    assert!(!text.lines().any(|l| l.starts_with("- ")), "{text}");
    let sqlite = render(&inputs(Vec::new(), ""));
    assert!(!sqlite.contains("services:") && !sqlite.contains("defaults:"));
}

#[test]
fn only_a_generated_workflow_is_ever_replaced() {
    let ours = format!("{MARKER}\nname: old\n");
    assert!(may_replace(None, true).is_ok());
    assert!(may_replace(Some(&ours), true).is_ok());
    assert!(may_replace(Some("name: mine\n"), false).is_ok());
    let err = may_replace(Some("name: mine\n"), true).unwrap_err();
    assert!(err.to_string().contains("never replaced"), "{err}");
    let words = |w: &[&str]| -> Vec<String> {
        let owned = w.iter().map(|s| (*s).to_owned());
        owned.collect()
    };
    assert!(parse(&words(&["--update"])).unwrap().update);
    assert!(parse(&words(&["--force"])).is_err());
}
