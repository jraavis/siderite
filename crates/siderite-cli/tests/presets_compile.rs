//! An app generated for SQLite still compiles after `siderite add
//! --preset postgres`, and the preset only adds the feature.
//!
//! Slow (it type-checks the framework with PostgreSQL support), so it is
//! ignored by default; the live CI job runs it with `--include-ignored`.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;

#[test]
#[ignore = "slow: compiles a generated project"]
fn a_preset_keeps_the_app_compiling() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent().unwrap().parent().unwrap();
    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("siderite presets {id}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let siderite = |args: &[&str], cwd: &Path| {
        let out = Command::new(env!("CARGO_BIN_EXE_siderite"))
            .args(args)
            .current_dir(cwd)
            .env("CARGO_TARGET_DIR", repo.join("target"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
    };
    let repo_arg = repo.display().to_string();
    let new = ["new", "app", "--database", "sqlite", "--path", &repo_arg];
    siderite(&new, &dir);
    let app = dir.join("app");
    let before = std::fs::read_to_string(app.join("Cargo.toml")).unwrap();
    siderite(&["add", "--preset", "postgres"], &app);
    let after = std::fs::read_to_string(app.join("Cargo.toml")).unwrap();
    let cli = after.lines().find(|l| l.starts_with("siderite-cli"));
    let cli = cli.unwrap();
    assert!(cli.contains("path = "), "the source is kept: {cli}");
    assert!(cli.contains("features = [\"postgres\"]"), "{cli}");
    let other = |text: &str| -> Vec<String> {
        let lines = text.lines().filter(|l| !l.starts_with("siderite-cli"));
        lines.map(str::to_owned).collect()
    };
    assert_eq!(other(&before), other(&after), "nothing else changes");
    let check = Command::new("cargo")
        .args(["check", "--quiet"])
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", repo.join("target"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&check.stderr);
    assert!(check.status.success(), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}
