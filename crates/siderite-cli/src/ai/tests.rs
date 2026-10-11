//! `ai dump-context` through [`AppCli`](crate::AppCli).

#![allow(clippy::unwrap_used)]

use super::*;
use crate::AppCli;
use crate::app_cli::Env;
use crate::fixtures::{ScratchDir, author, block_on, book};
use serde_json::Value;
use siderite_core::{App, get};

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

fn app() -> App {
    App::new()
        .title("Shop")
        .description("Uses postgres://app:hunter2@db/shop")
        .route("/health", get(|| async { "ok" }).operation_id("health"))
        .route("/secret", get(|| async { "s" }).hidden())
}

/// Run `list` against a CLI; the exit code and what reached the channel.
fn run(cli: &AppCli, list: &[&str]) -> (u8, String) {
    let scratch = ScratchDir::new("ai-run");
    let out = scratch.path().join("out");
    let env = Env {
        report_path: Some(out.clone()),
        ..Env::default()
    };
    let outcome = block_on(cli.execute(&args(list), &env));
    let code = outcome.unwrap_or_else(|err| err.exit_code());
    (code, std::fs::read_to_string(out).unwrap_or_default())
}

fn cli(url: &str) -> AppCli {
    let settings = CliSettings::new().database("default", url);
    AppCli::new(app)
        .models(&[author(), book()])
        .settings(settings)
}

#[test]
fn parses_formats_and_limits() {
    let parsed = parse(&args(&["ai", "dump-context"]), false).unwrap();
    assert_eq!(parsed.format, Format::Markdown);
    assert!(!parsed.live);
    let list = ["ai", "dump-context", "--format=json", "--live"];
    let parsed = parse(&args(&list), false).unwrap();
    assert_eq!((parsed.format, parsed.live), (Format::Json, true));
    let list = ["ai", "dump-context", "--max-bytes", "4096"];
    assert_eq!(parse(&args(&list), true).unwrap().limits.max_bytes, 4096);
    for bad in [
        ["ai"].as_slice(),
        &["ai", "init"],
        &["ai", "dump-context", "--format", "yaml"],
        &["ai", "dump-context", "--max-bytes", "0"],
    ] {
        assert_eq!(parse(&args(bad), false).unwrap_err().exit_code(), 2);
    }
}

#[test]
fn offline_context_labels_unknowns_and_hides_secrets() {
    let cli = cli("postgres://app:hunter3@db/app");
    let (code, text) = run(&cli, &["ai", "dump-context"]);
    assert_eq!(code, 0);
    assert!(text.starts_with("# Shop 0.1.0\n"), "{text}");
    assert!(text.contains("| default | postgres |"));
    assert!(text.contains("### Author (table `authors`)"));
    assert!(text.contains("| GET | /health | health |"));
    assert!(text.contains("Status unknown: offline"));
    assert!(text.contains("built at Unix time"));
    assert!(!text.contains("/secret"));
    for secret in ["hunter2", "hunter3", "db/app"] {
        assert!(!text.contains(secret), "{secret} leaked:\n{text}");
    }
    let (code, json) = run(&cli, &["--json", "ai", "dump-context"]);
    assert_eq!(code, 0);
    let value: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["command"], "ai dump-context");
    assert_eq!(value["data"]["migrations"]["state"], "unknown");
    assert_eq!(value["data"]["snapshot"]["format"], "siderite-snapshot");
    assert!(!json.contains("hunter"));
}

#[test]
fn a_failed_live_read_is_labelled_and_redacted() {
    // Rejected before any I/O, with a password in the URL.
    let cli = cli("ftp://root:hunter4@db/app");
    let list = ["ai", "dump-context", "--live", "--format", "json"];
    let (code, json) = run(&cli, &list);
    assert_eq!(code, 1);
    let value: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["data"]["migrations"]["state"], "failed");
    assert_eq!(value["diagnostics"][0]["code"], "ai.E001");
    assert!(!json.contains("hunter4"), "{json}");
    let (_, text) = run(&cli, &["ai", "dump-context", "--live"]);
    assert!(text.contains("Status failed"), "{text}");
    assert!(text.contains("[ai.E001]") && !text.contains("hunter4"));
}

#[test]
fn a_live_read_reports_the_status() {
    let scratch = ScratchDir::new("ai-live");
    let db = scratch.path().join("app.db");
    let url = format!("sqlite://{}?mode=rwc", db.display());
    let cli = cli(&url).migrations_dir(scratch.path().join("migrations"));
    let (code, text) = run(&cli, &["ai", "dump-context", "--live"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("Read from `default`: 0 applied, 0 pending."));
}

#[test]
fn oversized_metadata_is_cut_and_said_so() {
    let cli = cli("sqlite::memory:");
    let list = ["ai", "dump-context", "--max-bytes", "900"];
    let (code, text) = run(&cli, &list);
    assert_eq!(code, 0, "truncation is a warning");
    assert!(text.contains("**Incomplete:**"), "{text}");
    assert!(text.contains("[snapshot.W001]"), "{text}");
    let list = ["ai", "dump-context", "--max-bytes=900", "--format=json"];
    let (_, json) = run(&cli, &list);
    let value: Value = serde_json::from_str(&json).unwrap();
    let models = &value["data"]["snapshot"]["completeness"]["models"];
    let routes = &value["data"]["snapshot"]["completeness"]["routes"];
    assert!(models["complete"] == false || routes["complete"] == false);
}
