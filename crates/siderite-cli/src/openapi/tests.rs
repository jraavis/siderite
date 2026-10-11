//! `openapi export` through [`AppCli`](crate::AppCli), end to end.

#![allow(clippy::unwrap_used)]

use crate::app_cli::Env;
use crate::fixtures::ScratchDir;
use crate::{AppCli, CliSettings};
use serde_json::Value;
use siderite_core::{App, DocsConfig, get, post};

const META_SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../siderite-openapi/tests/fixtures/openapi-3.1-schema.json"
));

fn app() -> App {
    let v1 = App::new().route(
        "/users",
        post(|| async { "c" })
            .operation_id("create_user")
            .get(|| async { "l" }),
    );
    App::new()
        .title("Export")
        .route("/health", get(|| async { "ok" }).operation_id("health"))
        .route("/secret", get(|| async { "s" }).hidden())
        .mount("/api/v1", v1)
}

fn no_docs() -> App {
    app().docs(DocsConfig {
        openapi_url: None,
        swagger_url: None,
        redoc_url: None,
    })
}

/// Run `args`; return the exit code and what reached the channel.
fn run(cli: &AppCli, args: &[&str]) -> (u8, String) {
    let scratch = ScratchDir::new("openapi-run");
    let path = scratch.path().join("out");
    let env = Env {
        report_path: Some(path.clone()),
        ..Env::default()
    };
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let code = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(cli.execute(&args, &env))
        .unwrap_or_else(|err| err.exit_code());
    (code, std::fs::read_to_string(&path).unwrap_or_default())
}

fn assert_openapi_31(text: &str) -> Value {
    let doc: Value = serde_json::from_str(text).unwrap();
    let schema: Value = serde_json::from_str(META_SCHEMA).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator.iter_errors(&doc).collect();
    assert!(errors.is_empty(), "{errors:?}");
    doc
}

#[test]
fn exports_mounts_without_hidden_routes() {
    // An unreachable database proves no connection is attempted.
    let url = "postgres://u:p@127.0.0.1:1/none";
    let settings = CliSettings::new().database("default", url);
    let cli = AppCli::new(app).settings(settings);
    let (code, text) = run(&cli, &["openapi", "export"]);
    assert_eq!(code, 0);
    assert!(text.ends_with("}\n"), "{text}");
    let doc = assert_openapi_31(&text);
    let paths = doc["paths"].as_object().unwrap();
    let keys: Vec<_> = paths.keys().map(String::as_str).collect();
    assert_eq!(keys, ["/api/v1/users", "/health"]);
    assert_eq!(doc, app().openapi().unwrap().to_value());
    assert_eq!(doc["info"]["title"], "Export");
}

#[test]
fn exports_with_http_docs_disabled() {
    let (code, text) = run(&AppCli::new(no_docs), &["openapi", "export"]);
    assert_eq!(code, 0);
    assert_eq!(
        assert_openapi_31(&text),
        app().openapi().unwrap().to_value()
    );
}

#[test]
fn conflicting_operations_fail_without_output() {
    let dup = AppCli::new(|| {
        App::new()
            .route("/a", get(|| async { "a" }).operation_id("same"))
            .route("/b", get(|| async { "b" }).operation_id("same"))
    });
    let (code, text) = run(&dup, &["openapi", "export"]);
    assert_eq!(code, 1);
    assert!(text.is_empty());
    let (code, _) = run(&dup, &["openapi", "export", "--json"]);
    assert_eq!(code, 2, "--json is not an export option");
}

#[test]
fn file_output_is_atomic_and_never_silently_replaced() {
    let scratch = ScratchDir::new("openapi-file");
    let dest = scratch.path().join("spec file.json");
    let shown = dest.display().to_string();
    let cli = AppCli::new(app);
    let args = ["openapi", "export", "--output", shown.as_str()];
    let (code, text) = run(&cli, &args);
    assert_eq!((code, text.as_str()), (0, ""));
    let first = std::fs::read_to_string(&dest).unwrap();
    assert_openapi_31(&first);
    std::fs::write(&dest, "user edits").unwrap();
    let (code, _) = run(&cli, &args);
    assert_eq!(code, 2);
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "user edits");
    let forced = ["openapi", "export", "-o", shown.as_str(), "--force"];
    assert_eq!(run(&cli, &forced).0, 0);
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), first);
    let names: Vec<_> = std::fs::read_dir(scratch.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "temporary files left: {names:?}");
}
