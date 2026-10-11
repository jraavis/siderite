//! A project written by `siderite new --path <this checkout>` compiles.
//!
//! Slow (it type-checks the framework for the new crate), so it is
//! ignored by default; the live CI job runs it with `--include-ignored`.
//! It shares this workspace's target directory to reuse built crates.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;

#[test]
#[ignore = "slow: compiles a generated project"]
fn new_projects_compile_against_local_sources() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent().unwrap().parent().unwrap();
    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("siderite new compiles {id}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for engine in ["sqlite", "postgres"] {
        let name = format!("app_{engine}");
        let out = Command::new(env!("CARGO_BIN_EXE_siderite"))
            .args(["new", &name, "--database", engine, "--path"])
            .arg(repo)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let check = Command::new("cargo")
            .args(["check", "--quiet"])
            .current_dir(dir.join(&name))
            .env("CARGO_TARGET_DIR", repo.join("target"))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&check.stderr);
        assert!(check.status.success(), "{engine}: {stderr}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
