//! `config show` and `config validate`: arguments, sources, redaction and
//! diagnostics.

#![allow(clippy::unwrap_used)]

use super::*;
use crate::fixtures::{ScratchDir, assert_schema_valid};
use serde_json::json;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

fn options(words: &[&str]) -> Result<Options, CliError> {
    parse(&args(words))
}

fn project(toml: &str) -> ScratchDir {
    let dir = ScratchDir::new("config");
    std::fs::write(dir.path().join(FILE), toml).unwrap();
    dir
}

fn codes(report: &Report) -> Vec<&str> {
    report.diagnostics.iter().map(Diagnostic::code).collect()
}

const SHOW: Options = Options {
    action: Action::Show,
    json: false,
};
const VALIDATE: Options = Options {
    action: Action::Validate,
    json: true,
};

#[test]
fn arguments() {
    assert_eq!(options(&["config", "show", "--redacted"]).unwrap(), SHOW);
    let validate = options(&["config", "validate", "--json"]).unwrap();
    assert_eq!(validate, VALIDATE);
    for bad in [
        &["config"][..],
        &["config", "edit"],
        &["config", "show", "--plain"],
        &["config", "validate", "--redacted"],
    ] {
        assert!(options(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn show_names_sources_and_redacts() {
    let dir = project(
        "[app]\nname = \"blog\"\nsecret_key = \"s3cret\"\n\n\
         [databases.default]\nurl = \"postgres://u:hunter2@db/blog\"\n",
    );
    let report = report(SHOW, dir.path(), false);
    assert!(report.ok, "{:?}", report.diagnostics);
    let text = render(&report, Action::Show);
    assert!(
        !text.contains("hunter2") && !text.contains("s3cret"),
        "{text}"
    );
    assert!(text.contains("app.name = \"blog\"  (file "), "{text}");
    assert!(text.contains("log.level = \"info\"  (default)"), "{text}");
    let json = serde_json::to_value(&report).unwrap();
    let rendered = json.to_string();
    assert!(!rendered.contains("hunter2") && !rendered.contains("s3cret"));
    assert_eq!(json["data"]["file_exists"], json!(true));
    assert_schema_valid(&json);
}

#[test]
fn validate_reports_load_and_url_errors() {
    let empty = ScratchDir::new("config");
    let fine = report(VALIDATE, empty.path(), false);
    assert!(fine.ok && fine.data.entries.is_empty());
    let text = render(&fine, Action::Validate);
    assert!(
        text.contains("(absent)") && text.contains("is valid"),
        "{text}"
    );

    let bad_type = project("[server]\naddr = 5\n");
    let failed = report(VALIDATE, bad_type.path(), false);
    assert_eq!(codes(&failed), ["config.E004"]);
    assert_eq!(failed.exit_code(), 1);

    let bad_url = project(
        "[databases.default]\nurl = \"ftp://u:hunter2@h/x\"\n\n\
         [databases.other]\nurl = \"no-scheme\"\n",
    );
    let failed = report(VALIDATE, bad_url.path(), false);
    assert_eq!(codes(&failed), ["config.E003", "config.E002"]);
    let json = serde_json::to_value(&failed).unwrap();
    assert!(!json.to_string().contains("hunter2"), "{json}");
    assert_schema_valid(&json);
}
