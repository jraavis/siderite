//! `ai validate`: findings from a `verify` run.

#![allow(clippy::unwrap_used)]

use super::validate::{LIMITS, parse, render, report};
use crate::cargo_msg::{CompilerDiagnostic, Span};
use crate::fixtures::assert_schema_valid;
use crate::report::{Diagnostic, Envelope, Severity};
use crate::verify::{Status, Step, VerifyData};

fn step(name: &'static str, status: Status) -> Step {
    Step {
        name,
        command: format!("cargo {name}"),
        status,
        duration_ms: 1,
        exit_code: Some(i32::from(status == Status::Failed)),
        output: None,
        diagnostics: Vec::new(),
        omitted_diagnostics: 0,
        framework_diagnostics: Vec::new(),
    }
}

fn compiler(level: &str, code: &str) -> CompilerDiagnostic {
    CompilerDiagnostic {
        level: level.to_owned(),
        code: Some(code.to_owned()),
        message: format!("{code} message"),
        spans: vec![Span {
            file: "src/main.rs".to_owned(),
            line_start: 3,
            line_end: 3,
            column_start: 5,
            column_end: 9,
            primary: true,
            label: None,
        }],
        rendered: None,
    }
}

fn diag(code: &str, severity: Severity, at: &str) -> Diagnostic {
    Diagnostic::new(code, severity, "found").with_location(at)
}

fn run(steps: Vec<Step>) -> Envelope<VerifyData> {
    let failed = steps.iter().filter(|s| s.status == Status::Failed);
    let diags = failed
        .map(|s| Diagnostic::new("verify.E001", Severity::Error, s.name))
        .collect();
    let data = VerifyData {
        profile: "offline",
        diagnostic_format: 1,
        test_results: "aggregate",
        steps,
    };
    Envelope::new("verify", data, diags)
}

#[test]
fn findings_carry_locations_explanations_and_commands() {
    let mut build = step("build", Status::Failed);
    build.diagnostics = vec![
        compiler("warning", "unused_variables"),
        compiler("error", "E0308"),
    ];
    let mut check = step("check", Status::Failed);
    check.framework_diagnostics = vec![
        diag("migrations.W001", Severity::Warning, "models"),
        diag("models.E003", Severity::Error, "Loose"),
    ];
    let fmt = step("fmt", Status::Failed);
    let steps = vec![fmt, step("lint", Status::Passed), build, check];
    let out = report(run(steps));
    assert!(!out.ok);
    let data = &out.data;
    let summary: Vec<(&str, Option<&str>, Option<&str>)> = data
        .findings
        .iter()
        .map(|f| (f.step, f.code.as_deref(), f.location.as_deref()))
        .collect();
    assert_eq!(
        summary,
        [
            ("fmt", None, None),
            ("build", Some("E0308"), Some("src/main.rs:3:5")),
            ("check", Some("models.E003"), Some("Loose")),
            ("check", Some("migrations.W001"), Some("models")),
        ]
    );
    let rustc = data.findings[1].explanation.as_ref().unwrap();
    assert!(rustc.url.as_deref().unwrap().ends_with("/E0308.html"));
    let framework = data.findings[2].explanation.as_ref().unwrap();
    assert_eq!(framework.docs, Some("guides/production/cli#check"));
    assert_eq!(data.findings[1].reproduce, "cargo build");
    let failed = ["cargo fmt", "cargo build", "cargo check"];
    assert_eq!(data.commands[..3], failed);
    assert_eq!(data.commands[3], "siderite verify");
    assert_eq!(data.limits, LIMITS);
    assert_schema_valid(&serde_json::to_value(&out).unwrap());
    let text = render(&out);
    assert!(text.starts_with("ai validate (offline): 3 of 4 steps failed"));
    assert!(
        text.contains("2. error[E0308] src/main.rs:3:5 (build)"),
        "{text}"
    );
    assert!(
        text.contains("   see: https://doc.rust-lang.org/"),
        "{text}"
    );
    assert!(text.trim_end().ends_with("acceptance tests."), "{text}");
}

#[test]
fn a_clean_run_still_states_its_limits() {
    let out = report(run(vec![step("fmt", Status::Passed)]));
    assert!(out.ok);
    assert!(out.data.findings.is_empty());
    assert_eq!(out.data.commands, ["siderite verify"]);
    assert!(render(&out).contains(LIMITS));
}

#[test]
fn findings_are_bounded() {
    let mut build = step("build", Status::Failed);
    build.diagnostics = (0..60)
        .map(|i| {
            let mut d = compiler("error", "E0308");
            d.message = format!("m{i}");
            d
        })
        .collect();
    let out = report(run(vec![build]));
    assert_eq!(out.data.findings.len(), 50);
    assert_eq!(out.data.omitted_findings, 10);
}

#[test]
fn parses_only_its_flags() {
    let args = |l: &[&str]| -> Vec<String> {
        let owned = l.iter().map(|a| (*a).to_owned());
        owned.collect()
    };
    let parsed = parse(&args(&["--json", "ai", "validate", "--live"]));
    let parsed = parsed.unwrap();
    assert!(parsed.json && parsed.live);
    assert!(parse(&args(&["ai", "validate", "--fix"])).is_err());
}

#[test]
fn the_same_error_from_lint_and_build_is_one_finding() {
    let mut lint = step("lint", Status::Failed);
    lint.diagnostics = vec![compiler("error", "E0308")];
    let mut build = step("build", Status::Failed);
    build.diagnostics = vec![compiler("error", "E0308")];
    let out = report(run(vec![lint, build]));
    assert_eq!(out.data.findings.len(), 1);
    assert_eq!(out.data.findings[0].reproduce, "cargo lint");
}
