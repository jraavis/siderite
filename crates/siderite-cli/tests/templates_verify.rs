//! Every `siderite new` template produces a project that passes
//! `siderite verify` (fmt, lint, build, test and framework checks).
//!
//! Slow (it builds each generated project), so it is ignored by default;
//! the live CI job runs it with `--include-ignored`. Projects share this
//! workspace's target directory to reuse built crates.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;

#[test]
#[ignore = "slow: builds and verifies generated projects"]
fn every_template_passes_verify() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent().unwrap().parent().unwrap();
    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("siderite templates {id}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let siderite = env!("CARGO_BIN_EXE_siderite");
    for template in ["default", "api"] {
        let name = format!("app_{template}");
        let repo_arg = repo.display().to_string();
        let args = ["new", &name, "--template", template, "--path", &repo_arg];
        let made = Command::new(siderite)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        let project = dir.join(&name);
        let recorded = project.join(".siderite/template.toml");
        let manifest = std::fs::read_to_string(recorded).unwrap();
        assert!(
            manifest.contains(&format!("name = \"{template}\"")),
            "{manifest}"
        );
        let verified = Command::new(siderite)
            .args(["verify", "--json"])
            .current_dir(&project)
            .env("CARGO_TARGET_DIR", repo.join("target"))
            .env_remove("DATABASE_URL")
            .output()
            .unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&verified.stdout).unwrap_or_default();
        assert_eq!(report["ok"], true, "{template}: {report:#}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
