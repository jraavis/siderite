//! Golden, redaction, schema and exit-code tests of the output contract.

#![allow(clippy::unwrap_used)]

use super::*;
use crate::app_cli::Env;
use crate::check::{CheckIssue, check, check_build, has_errors};
use crate::fixtures::{author, book, no_pk};
use crate::settings::CliSettings;
use crate::{AppCli, routes};
use serde_json::Value;
use siderite_core::{App, get, post};
use std::path::PathBuf;

fn app() -> App {
    let v1 = App::new()
        .route(
            "/users",
            post(|| async { "c" })
                .operation_id("create_user")
                .get(|| async { "l" }),
        )
        .route("/", get(|| async { "root" }));
    App::new()
        .route("/health", get(|| async { "ok" }).operation_id("health"))
        .route("/secret", get(|| async { "s" }).hidden())
        .mount("/api/v1", v1)
}

fn empty_dir(tag: &str) -> PathBuf {
    let name = format!("siderite-cli-report-{tag}-{}", std::process::id());
    let dir = std::env::temp_dir().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The pre-refactor `AppCli::check` output, kept as the reference.
fn legacy_check_text(issues: &[CheckIssue]) -> String {
    let mut out = String::new();
    for issue in issues {
        out.push_str(&format!("{issue}\n"));
    }
    if issues.is_empty() {
        out.push_str("System check identified no issues.\n");
    } else {
        out.push_str(&format!(
            "System check identified {} issue(s).\n",
            issues.len()
        ));
    }
    out
}

fn legacy_issues(
    models: &[&'static siderite_orm::ModelMeta],
    settings: &CliSettings,
    dir: &std::path::Path,
) -> Vec<CheckIssue> {
    let mut issues = check(&app(), models, settings, Some(dir));
    if !issues.iter().any(|i| i.id.starts_with("openapi.")) {
        issues.extend(check_build(app()));
    }
    issues
}

#[test]
fn check_text_matches_the_historical_output() {
    let dir = empty_dir("golden");
    let none = CliSettings::new();
    let clean = build::check_report(&app, &[], &none, Some(&dir));
    assert_eq!(
        text::render_check(&clean),
        "System check identified no issues.\n"
    );

    let settings = CliSettings::new().database("default", "sqlite::memory:");
    let models = [author(), book(), no_pk()];
    let report = build::check_report(&app, &models, &settings, Some(&dir));
    let text = text::render_check(&report);
    assert_eq!(
        text,
        "error: [models.E003] model `Loose` has no primary key\n\
         warning: [migrations.W001] models have 3 change(s) that no \
         migration records; run `makemigrations`\n\
         System check identified 2 issue(s).\n"
    );
    let issues = legacy_issues(&models, &settings, &dir);
    assert_eq!(text, legacy_check_text(&issues));
    assert_eq!(report.exit_code(), u8::from(has_errors(&issues)));
}

#[test]
fn routes_text_matches_the_historical_table() {
    let reports = build::route_reports(&app()).unwrap();
    assert_eq!(
        text::render_routes(&reports),
        "GET   /api/v1        -\n\
         GET   /api/v1/users  -\n\
         POST  /api/v1/users  create_user\n\
         GET   /health        health\n"
    );
    let rows = routes::route_table(&app()).unwrap();
    assert_eq!(routes::render_routes(&rows), text::render_routes(&reports));
    assert_eq!(text::render_routes(&[]), "No routes.\n");
}

#[test]
fn reports_are_sorted_and_stable() {
    let first = json::render(&build::routes(&app()).unwrap()).unwrap();
    let second = json::render(&build::routes(&app()).unwrap()).unwrap();
    assert_eq!(first, second);
    let a = build::models(&[book(), author()]);
    let b = build::models(&[author(), book()]);
    assert_eq!(a, b);
    let names: Vec<_> = a.data.models.iter().map(|m| &m.name).collect();
    assert_eq!(names, ["Author", "Book"]);
    let first = &a.data.models[0].fields;
    let fields: Vec<_> = first.iter().map(|f| &f.name).collect();
    assert_eq!(fields, ["id", "name"]);
}

#[test]
fn json_is_one_value_and_a_newline_with_a_fixed_shape() {
    let report = build::routes(&app()).unwrap();
    let out = json::render(&report).unwrap();
    assert!(out.ends_with('\n'));
    assert_eq!(out.matches('\n').count(), 1);
    let head = "{\"schema_version\":1,\"command\":\"routes\",";
    assert!(out.starts_with(head));
    assert!(out.contains("\"ok\":true,\"data\":{\"routes\":["));
    assert!(out.ends_with(",\"diagnostics\":[]}\n"));
    let back: Envelope<RoutesData> = serde_json::from_str(&out).unwrap();
    assert_eq!(back, report);
}

#[test]
fn diagnostics_redact_connection_strings() {
    let d = Diagnostic::new(
        "x.E001",
        Severity::Error,
        "cannot open postgres://app:hunter2@db:5432/x?sslmode=require&\
         password=hunter3 now",
    )
    .with_location("mysql://root:hunter4@h/db")
    .with_hint("redis://token123@h");
    let json = serde_json::to_string(&d).unwrap();
    for secret in ["hunter2", "hunter3", "hunter4", "token123"] {
        assert!(!json.contains(secret), "{json}");
    }
    assert_eq!(
        d.message,
        "cannot open postgres://app:***@db:5432/x?sslmode=require&\
         password=*** now"
    );
    assert_eq!(redact_field("api_key", "abc"), "***");
    assert_eq!(redact_field("name", "plain"), "plain");
    assert_eq!(redact_text("no url here"), "no url here");
    assert_eq!(redact_text("sqlite::memory:"), "sqlite::memory:");
}

#[test]
fn outputs_validate_against_the_published_schema() {
    const SCHEMA: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/schemas/cli-report-v1.json"
    ));
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let dir = empty_dir("schema");
    let settings = CliSettings::new().database("default", "sqlite::memory:");
    let samples = [
        json::render(&build::routes(&app()).unwrap()).unwrap(),
        json::render(&build::models(&[author(), book()])).unwrap(),
        json::render(&build::check_report(
            &app,
            &[author(), book(), no_pk()],
            &settings,
            Some(&dir),
        ))
        .unwrap(),
        json::render(&Envelope::new(
            "check",
            CheckSummary {
                errors: 0,
                warnings: 1,
            },
            vec![
                Diagnostic::new("a.W001", Severity::Warning, "m")
                    .with_location("l")
                    .with_hint("h"),
            ],
        ))
        .unwrap(),
    ];
    for sample in &samples {
        let value: Value = serde_json::from_str(sample).unwrap();
        let errors: Vec<_> = validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{errors:?}\n{sample}");
    }
    // The schema rejects a wrong version and a mistyped payload.
    let sample = &samples[0];
    for (from, to) in [
        ("\"schema_version\":1", "\"schema_version\":2"),
        ("\"routes\":[", "\"routes\":[1,"),
    ] {
        let text = sample.replace(from, to);
        let bad: Value = serde_json::from_str(&text).unwrap();
        assert!(!validator.is_valid(&bad), "{to}");
    }
}

fn cli() -> AppCli {
    AppCli::new(app)
}

fn code(cli: &AppCli, args: &[&str]) -> u8 {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let env = Env::default();
    match tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(cli.execute(&args, &env))
    {
        Ok(code) => code,
        Err(err) => err.exit_code(),
    }
}

#[test]
fn exit_code_snapshots() {
    let dir = empty_dir("exit");
    assert_eq!(code(&cli().migrations_dir(&dir), &["check"]), EXIT_OK);
    assert_eq!(code(&cli(), &["routes"]), EXIT_OK);
    let failing = cli().models(&[no_pk()]).migrations_dir(&dir);
    assert_eq!(code(&failing, &["check"]), EXIT_FAILURE);
    assert_eq!(code(&cli(), &["check", "--nope"]), EXIT_USAGE);
    assert_eq!(code(&cli(), &["frobnicate"]), EXIT_USAGE);
    assert_eq!(code(&cli(), &[]), EXIT_USAGE);
}
