//! Golden, redaction, schema and exit-code tests of the output contract.

#![allow(clippy::unwrap_used)]

use super::*;
use crate::app_cli::Env;
use crate::check::{CheckIssue, check, check_build, has_errors};
use crate::fixtures::{ScratchDir, author, book, no_pk};
use crate::settings::CliSettings;
use crate::{AppCli, routes};
use serde_json::Value;
use siderite_core::{App, get, post};

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
    let scratch = ScratchDir::new("report-golden");
    let dir = scratch.path();
    let none = CliSettings::new();
    let clean = build::check_report(&app, &[], &none, Some(dir));
    assert_eq!(
        text::render_check(&clean),
        "System check identified no issues.\n"
    );

    let settings = CliSettings::new().database("default", "sqlite::memory:");
    let models = [author(), book(), no_pk()];
    let report = build::check_report(&app, &models, &settings, Some(dir));
    let text = text::render_check(&report);
    assert_eq!(
        text,
        "error: [models.E003] model `Loose` has no primary key\n\
         warning: [migrations.W001] models have 3 change(s) that no \
         migration records; run `makemigrations`\n\
         System check identified 2 issue(s).\n"
    );
    let issues = legacy_issues(&models, &settings, dir);
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
        d.message(),
        "cannot open postgres://app:***@db:5432/x?sslmode=require&\
         password=*** now"
    );
    assert_eq!(redact_field("api_key", "abc"), "***");
    assert_eq!(redact_field("name", "plain"), "plain");
    assert_eq!(redact_text("no url here"), "no url here");
    assert_eq!(redact_text("sqlite::memory:"), "sqlite::memory:");
}

const SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/schemas/cli-report-v1.json"
));

fn validator() -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

fn assert_valid(validator: &jsonschema::Validator, value: &Value) {
    let errors: Vec<_> = validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}\n{value}");
}

#[test]
fn outputs_validate_against_the_published_schema() {
    let validator = validator();
    let scratch = ScratchDir::new("report-schema");
    let dir = scratch.path();
    let settings = CliSettings::new().database("default", "sqlite::memory:");
    let samples = [
        json::render(&build::routes(&app()).unwrap()).unwrap(),
        json::render(&build::models(&[author(), book()])).unwrap(),
        json::render(&build::check_report(
            &app,
            &[author(), book(), no_pk()],
            &settings,
            Some(dir),
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
        assert_valid(&validator, &serde_json::from_str(sample).unwrap());
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
    let scratch = ScratchDir::new("report-exit");
    let dir = scratch.path();
    assert_eq!(code(&cli().migrations_dir(dir), &["check"]), EXIT_OK);
    assert_eq!(code(&cli(), &["routes"]), EXIT_OK);
    let failing = cli().models(&[no_pk()]).migrations_dir(dir);
    assert_eq!(code(&failing, &["check"]), EXIT_FAILURE);
    assert_eq!(code(&cli(), &["check", "--nope"]), EXIT_USAGE);
    assert_eq!(code(&cli(), &["frobnicate"]), EXIT_USAGE);
    assert_eq!(code(&cli(), &[]), EXIT_USAGE);
}

#[test]
fn deserialized_diagnostics_are_redacted_on_output() {
    let raw = r#"{"code":"x.E001","severity":"error",
        "message":"postgres://u:hunter2@h","location":"password=hunter3",
        "hint":"Pwd=hunter4"}"#;
    let d: Diagnostic = serde_json::from_str(raw).unwrap();
    let json = serde_json::to_string(&d).unwrap();
    let line = text::diagnostic_line(&d);
    for secret in ["hunter2", "hunter3", "hunter4"] {
        assert!(!json.contains(secret), "{json}");
        assert!(!line.contains(secret), "{line}");
    }
    assert_eq!(line, "error: [x.E001] postgres://u:***@h");
}

#[test]
fn every_check_code_matches_the_schema_pattern() {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let code = &schema["$defs"]["Diagnostic"]["properties"]["code"];
    let validator = jsonschema::validator_for(code).unwrap();
    let scratch = ScratchDir::new("report-codes");
    let settings = CliSettings::new()
        .database("default", "sqlite::memory:")
        .database("bad", "nope://x");
    let models = [author(), no_pk(), crate::fixtures::same_table()];
    let dir = Some(scratch.path());
    let report = build::check_report(&app, &models, &settings, dir);
    assert!(!report.diagnostics.is_empty());
    for d in &report.diagnostics {
        assert!(validator.is_valid(&Value::from(d.code())), "{}", d.code());
    }
    for bad in ["E001", "models.e001", "Models.E001", "models.X001"] {
        assert!(!validator.is_valid(&Value::from(bad)), "{bad}");
    }
}

/// Run `args` with reports going to a scratch file; return the exit code
/// and the parsed report (`Value::Null` when nothing was written).
fn json_run(cli: &AppCli, args: &[&str]) -> (u8, Value) {
    let scratch = ScratchDir::new("report-json");
    let path = scratch.path().join("report.json");
    let env = Env {
        report_path: Some(path.clone()),
        ..Env::default()
    };
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let outcome = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(cli.execute(&args, &env));
    let code = outcome.unwrap_or_else(|err| err.exit_code());
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    if text.is_empty() {
        return (code, Value::Null);
    }
    assert_eq!(text.matches('\n').count(), 1, "{text}");
    (code, serde_json::from_str(&text).unwrap())
}

#[test]
fn json_flag_works_before_and_after_the_command() {
    let validator = validator();
    for args in [["--json", "routes"], ["routes", "--json"]] {
        let (code, report) = json_run(&cli(), &args);
        assert_eq!(code, EXIT_OK);
        assert_valid(&validator, &report);
        assert_eq!(report["command"], "routes");
        let paths: Vec<_> = report["data"]["routes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["path"].as_str().unwrap().to_owned())
            .collect();
        assert!(paths.contains(&"/health".to_owned()), "{paths:?}");
        assert!(!paths.contains(&"/secret".to_owned()), "hidden route");
    }
}

#[test]
fn json_check_keeps_its_exit_code() {
    let validator = validator();
    let scratch = ScratchDir::new("report-json-check");
    let clean = cli().migrations_dir(scratch.path());
    let (code, report) = json_run(&clean, &["check", "--json"]);
    assert_eq!(code, EXIT_OK);
    assert_valid(&validator, &report);
    assert_eq!(report["ok"], true);
    let failing = cli().models(&[no_pk()]).migrations_dir(scratch.path());
    let (code, report) = json_run(&failing, &["--json", "check"]);
    assert_eq!(code, EXIT_FAILURE);
    assert_valid(&validator, &report);
    assert_eq!(report["ok"], false);
    assert!(report["data"]["errors"].as_u64().unwrap() >= 1);
    let codes = report["diagnostics"].as_array().unwrap();
    assert!(codes.iter().any(|d| d["code"] == "models.E003"), "{report}");
}

#[test]
fn json_failures_are_envelopes_with_exit_semantics() {
    let validator = validator();
    let cases: [(&[&str], &str); 7] = [
        (&["run", "--json"], "run"),
        (&["--json", "migrate"], "migrate"),
        (&["--json"], "siderite"),
        (&["routes", "--json", "--help"], "routes"),
        (&["routes", "--json=1"], "routes"),
        (&["check", "extra", "--json"], "check"),
        (&["--json", "frobnicate"], "frobnicate"),
    ];
    for (args, command) in cases {
        let (code, report) = json_run(&cli(), args);
        assert_eq!(code, EXIT_USAGE, "{args:?}");
        assert_valid(&validator, &report);
        assert_eq!(report["command"], command, "{args:?}");
        assert_eq!(report["ok"], false);
        assert!(report["data"].is_null());
        assert_eq!(report["diagnostics"][0]["code"], channel::USAGE_CODE);
    }
    // A failing command without a report is code cli.E002, exit 1.
    let broken = AppCli::new(|| {
        App::new()
            .route("/a", get(|| async { "a" }).operation_id("same"))
            .route("/b", get(|| async { "b" }).operation_id("same"))
    });
    let (code, report) = json_run(&broken, &["routes", "--json"]);
    assert_eq!(code, EXIT_FAILURE);
    assert_valid(&validator, &report);
    assert_eq!(report["diagnostics"][0]["code"], channel::FAILED_CODE);
}

#[test]
fn text_mode_writes_no_report() {
    let (code, report) = json_run(&cli(), &["routes"]);
    assert_eq!(code, EXIT_OK);
    assert!(report.is_null());
}
