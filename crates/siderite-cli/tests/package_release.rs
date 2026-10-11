//! `siderite package --release` on a generated project: the package has
//! the binary, the declared assets and migrations, a manifest whose
//! checksums match, nothing secret, and a binary that runs without Cargo.
//!
//! Slow (a release build), so it is ignored by default.

#![allow(clippy::unwrap_used)]

use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

fn run(dir: &Path, args: &[&str], target: &Path) -> std::process::Output {
    let siderite = env!("CARGO_BIN_EXE_siderite");
    Command::new(siderite)
        .args(args)
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target)
        .env_remove("DATABASE_URL")
        .output()
        .unwrap()
}

/// What the exclusion rules leave out of the test project.
const LEFT_OUT: [&str; 3] = [
    "migrations/.gitkeep", // a VCS placeholder from `siderite new`
    "static/.env",
    "static/dev.db",
];

#[test]
#[ignore = "slow: builds a release binary"]
fn a_release_package_is_complete_and_runs_without_cargo() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent().unwrap().parent().unwrap();
    let target = repo.join("target");
    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("siderite-package-{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let repo_arg = repo.display().to_string();
    let made = run(&dir, &["new", "shop", "--path", &repo_arg], &target);
    assert!(made.status.success(), "{made:?}");
    let project = dir.join("shop");
    let cargo_toml = project.join("Cargo.toml");
    let mut text = std::fs::read_to_string(&cargo_toml).unwrap();
    text.push_str("\n[package.metadata.siderite.package]\n");
    text.push_str("assets = [\"static\"]\n");
    std::fs::write(&cargo_toml, text).unwrap();
    for (rel, body) in [
        ("static/index.html", "<p>hi</p>"),
        ("static/.env", "SECRET=1"),
        ("static/dev.db", "x"),
        ("migrations/0001_initial.json", "{\"operations\": []}"),
    ] {
        let path = project.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let unlocked = run(&project, &["package", "--release"], &target);
    assert_eq!(unlocked.status.code(), Some(2), "{unlocked:?}");
    let built = run(&project, &["build"], &target);
    assert!(built.status.success(), "{built:?}");
    let out = dir.join("dist");
    let out_arg = out.display().to_string();
    let args = ["package", "--release", "--output", &out_arg, "--json"];
    let packaged = run(&project, &args, &target);
    let report = serde_json::from_slice(&packaged.stdout);
    let report: serde_json::Value = report.unwrap_or_default();
    assert_eq!(report["ok"], true, "{packaged:?}");
    let manifest = std::fs::read_to_string(out.join("manifest.json"));
    let manifest = serde_json::from_str(&manifest.unwrap());
    let manifest: serde_json::Value = manifest.unwrap();
    assert_eq!(manifest["locked"], true);
    assert_eq!(manifest["profile"], "release");
    let paths: Vec<&str> = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "bin/shop",
            "migrations/0001_initial.json",
            "static/index.html"
        ]
    );
    let excluded = &manifest["excluded"];
    assert_eq!(excluded, &serde_json::json!(LEFT_OUT));
    for file in manifest["files"].as_array().unwrap() {
        let bytes = std::fs::read(out.join(file["path"].as_str().unwrap()));
        let digest = Sha256::digest(bytes.unwrap());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(file["sha256"], hex.as_str());
    }
    let sums = std::fs::read_to_string(out.join("SHA256SUMS")).unwrap();
    assert_eq!(sums.lines().count(), 3);
    // The binary runs with nothing on PATH.
    let routes = Command::new(out.join("bin/shop"))
        .arg("routes")
        .current_dir(&out)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(routes.status.success(), "{routes:?}");
    let again = run(&project, &args, &target);
    assert_eq!(again.status.code(), Some(1), "{again:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
