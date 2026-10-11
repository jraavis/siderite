//! The application metadata snapshot: content, bounds and redaction.

#![allow(clippy::unwrap_used)]

use super::snapshot::{FORMAT, FORMAT_VERSION, Limits, Snapshot, snapshot};
use super::*;
use crate::fixtures::{author, book, no_pk};
use crate::settings::CliSettings;
use serde_json::Value;
use siderite_core::{App, get, post};

fn app() -> App {
    let v1 = App::new().route(
        "/users",
        post(|| async { "c" })
            .operation_id("create_user")
            .summary("Create; see postgres://u:hunter2@h/db")
            .get(|| async { "l" }),
    );
    App::new()
        .title("Shop")
        .description("Backed by mysql://root:hunter3@db/shop")
        .route("/health", get(|| async { "ok" }).operation_id("health"))
        .route("/secret", get(|| async { "s" }).hidden())
        .mount("/api/v1", v1)
}

fn settings() -> CliSettings {
    CliSettings::new()
        .database("default", "postgres://app:hunter4@db/app")
        .database("cache", "redis://:hunter5@cache")
        .database("odd", "ftp://x")
}

type Models<'a> = &'a [&'static siderite_orm::ModelMeta];

/// The snapshot of the test app with `models`.
fn build(models: Models<'_>, limits: Limits) -> Envelope<Snapshot> {
    snapshot(&app(), models, &settings(), limits)
}

fn validate(value: &Value) {
    let schema: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/schemas/cli-report-v1.json"
    )))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator.iter_errors(value).collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn a_complete_snapshot_without_secrets() {
    let report = build(&[book(), author()], Limits::default());
    assert!(report.ok);
    let data = &report.data;
    assert_eq!(
        (data.format.as_str(), data.format_version),
        (FORMAT, FORMAT_VERSION)
    );
    assert_eq!(data.framework.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(data.app.title, "Shop");
    let aliases: Vec<_> = data
        .databases
        .iter()
        .map(|d| (d.alias.as_str(), d.backend.as_str()))
        .collect();
    assert_eq!(
        aliases,
        [
            ("cache", "redis"),
            ("default", "postgres"),
            ("odd", "unknown")
        ]
    );
    let names: Vec<_> = data.models.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["Author", "Book"]);
    let paths: Vec<_> = data.routes.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, ["/api/v1/users", "/api/v1/users", "/health"]);
    assert!(data.openapi.is_some());
    assert!(data.completeness.models.complete);
    assert!(data.completeness.routes.complete);
    assert!(data.completeness.openapi.complete);
    let json = json::render(&report).unwrap();
    for secret in ["hunter2", "hunter3", "hunter4", "hunter5", "db/app"] {
        assert!(!json.contains(secret), "{secret} leaked");
    }
    assert!(!json.contains("/secret"), "hidden route");
    validate(&serde_json::from_str(&json).unwrap());
    // Stable: building twice gives the same bytes.
    let again = build(&[author(), book()], Limits::default());
    assert_eq!(json, json::render(&again).unwrap());
}

#[test]
fn limits_truncate_explicitly() {
    let limits = Limits {
        max_models: 1,
        max_routes: 2,
        max_openapi_bytes: 10,
        ..Limits::default()
    };
    let report = build(&[author(), book(), no_pk()], limits);
    assert!(report.ok, "truncation is a warning");
    let c = &report.data.completeness;
    assert_eq!(
        (c.models.complete, c.models.total, c.models.included),
        (false, Some(3), 1)
    );
    assert_eq!((c.routes.complete, c.routes.included), (false, 2));
    assert!(!c.openapi.complete && report.data.openapi.is_none());
    let diags = report.diagnostics.iter();
    let codes: Vec<_> = diags.map(Diagnostic::code).collect();
    assert_eq!(codes, ["snapshot.W001"; 3]);
    validate(&serde_json::to_value(&report).unwrap());
}

#[test]
fn the_byte_budget_drops_the_spec_then_routes() {
    let full = build(&[author(), book()], Limits::default());
    let without_spec = {
        let mut data = full.data.clone();
        data.openapi = None;
        serde_json::to_vec(&data).unwrap().len()
    };
    let limits = Limits {
        max_bytes: without_spec + 200,
        ..Limits::default()
    };
    let report = build(&[author(), book()], limits);
    assert!(report.data.openapi.is_none());
    assert!(report.data.completeness.routes.complete);
    // Reachable by dropping routes: met exactly.
    let budget = without_spec - 150;
    let limits = Limits {
        max_bytes: budget,
        ..Limits::default()
    };
    let report = build(&[author(), book()], limits);
    assert!(serde_json::to_vec(&report.data).unwrap().len() <= budget);
    assert!(!report.data.completeness.routes.complete);
    assert!(report.data.completeness.models.complete);
    // Unreachable: everything droppable goes, and the report says so.
    let tiny = Limits {
        max_bytes: 100,
        ..Limits::default()
    };
    let report = build(&[author(), book()], tiny);
    assert!(report.data.models.is_empty() && report.data.routes.is_empty());
    assert!(!report.data.completeness.models.complete);
    assert_eq!(report.diagnostics[0].code(), "snapshot.W001");
}

#[test]
fn a_broken_spec_is_an_explicit_error() {
    let broken = App::new()
        .route("/a", get(|| async { "a" }).operation_id("same"))
        .route("/b", get(|| async { "b" }).operation_id("same"));
    let none = CliSettings::new();
    let report = snapshot(&broken, &[author()], &none, Limits::default());
    assert!(!report.ok);
    assert_eq!(report.diagnostics[0].code(), "snapshot.E001");
    assert!(report.data.routes.is_empty() && report.data.openapi.is_none());
    assert!(!report.data.completeness.routes.complete);
    assert_eq!(report.data.models.len(), 1, "models still reported");
    validate(&serde_json::to_value(&report).unwrap());
}
