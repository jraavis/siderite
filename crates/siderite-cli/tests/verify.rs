//! `siderite verify` runs every step through a stand-in `cargo` and
//! reports them in one envelope (see `common`).

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

mod common;

use common::{fixture, one_value, scratch, script, siderite, stderr};
use serde_json::Value;
use std::path::Path;

/// A `cargo` that logs each call to `$LOG` and fails the subcommand named
/// by `$FAIL_SUB`, printing a credential-bearing URL as it does.
const CARGO: &str = r#"echo "$*" >> "$LOG"
if [ -n "$SIDERITE_REPORT_PATH" ]; then
  printf '%s\n' '{"ok":true}' > "$SIDERITE_REPORT_PATH"
fi
if [ "$1" = "$FAIL_SUB" ]; then
  echo "connecting to postgres://app:hunter2@db/app" >&2
  exit 3
fi
exit 0"#;

/// The package, a `PATH` with the stand-ins first, and the call log.
fn setup(root: &Path, components: bool) -> (std::path::PathBuf, String) {
    let (app, _) = fixture(root);
    let bin = root.join("bin");
    script(&bin, "cargo", CARGO);
    if components {
        script(&bin, "cargo-fmt", "exit 0");
        script(&bin, "cargo-clippy", "exit 0");
    }
    (app, format!("{}:/usr/bin:/bin", bin.display()))
}

fn run(root: &Path, args: &[&str], fail: &str) -> (Value, i32, Vec<String>) {
    let (app, path) = setup(root, true);
    let log = root.join("calls.txt");
    let log_path = log.display().to_string();
    let fake = [("LOG", log_path.as_str()), ("FAIL_SUB", fail)];
    let out = siderite(&app, &path, args, &fake);
    let report = one_value(&out);
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    let calls = calls.lines().map(str::to_owned).collect();
    (report, out.status.code().unwrap_or(-1), calls)
}

fn statuses(report: &Value) -> Vec<String> {
    let steps = report["data"]["steps"].as_array().unwrap();
    steps
        .iter()
        .map(|s| s["status"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn every_step_passes_in_order() {
    let root = scratch("verify-ok");
    let (report, code, calls) = run(&root.0, &["verify", "--json"], "");
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["command"], "verify");
    assert_eq!(report["ok"], true);
    assert_eq!(report["data"]["profile"], "offline");
    assert_eq!(statuses(&report), ["passed"; 5]);
    assert_eq!(
        calls,
        [
            "fmt --check",
            "clippy --all-targets -- -D warnings",
            "build --all-targets",
            "test --all-targets",
            "run -- check --json",
        ]
    );
}

#[test]
fn a_failed_build_skips_test_and_check() {
    let root = scratch("verify-build");
    let (report, code, calls) = run(&root.0, &["verify", "--json"], "build");
    assert_eq!(code, 1, "{report}");
    assert_eq!(report["ok"], false);
    let want = ["passed", "passed", "failed", "skipped", "skipped"];
    assert_eq!(statuses(&report), want);
    assert_eq!(calls.len(), 3, "{calls:?}");
    let build = &report["data"]["steps"][2];
    assert_eq!(build["exit_code"], 3);
    let output = build["output"].as_str().unwrap();
    assert!(output.contains("postgres://"), "{output}");
    assert!(!output.contains("hunter2"), "{output}");
    let diags = report["diagnostics"].as_array().unwrap();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0]["code"], "verify.E001");
    assert_eq!(diags[0]["location"], "build");
}

#[test]
fn a_failed_lint_still_runs_the_rest() {
    let root = scratch("verify-lint");
    let (report, code, _) = run(&root.0, &["verify", "--json"], "clippy");
    assert_eq!(code, 1);
    let want = ["passed", "failed", "passed", "passed", "passed"];
    assert_eq!(statuses(&report), want);
}

#[test]
fn live_includes_ignored_tests() {
    let root = scratch("verify-live");
    let args = ["verify", "--json", "--live"];
    let (report, code, calls) = run(&root.0, &args, "");
    assert_eq!(code, 0);
    assert_eq!(report["data"]["profile"], "live");
    assert!(calls.contains(&"test --all-targets -- --include-ignored".into()));
}

#[test]
fn a_missing_component_fails_the_step_with_the_fix() {
    let root = scratch("verify-component");
    let (app, path) = setup(&root.0, false);
    let log = root.0.join("calls.txt").display().to_string();
    let fake = [("LOG", log.as_str())];
    let out = siderite(&app, &path, &["verify", "--json"], &fake);
    let report = one_value(&out);
    assert_eq!(out.status.code(), Some(1));
    let fmt = &report["data"]["steps"][0]["output"];
    assert!(fmt.as_str().unwrap().contains("rustup component add"));
}

#[test]
fn text_output_and_usage_errors() {
    let root = scratch("verify-text");
    let (app, path) = setup(&root.0, true);
    let log = root.0.join("calls.txt").display().to_string();
    let fake = [("LOG", log.as_str()), ("FAIL_SUB", "test")];
    let out = siderite(&app, &path, &["verify"], &fake);
    assert_eq!(out.status.code(), Some(1));
    let text = common::stdout(&out);
    assert!(text.contains("FAILED   test"), "{text}");
    assert!(text.contains("1 of 5 steps failed"), "{text}");
    let out = siderite(&app, &path, &["verify", "--bogus"], &fake);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}
