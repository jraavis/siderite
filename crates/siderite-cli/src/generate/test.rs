//! `siderite generate test route NAME [--path /URL]` and `generate test
//! resource NAME [FIELD:TYPE[?]]... [--table TABLE]`: an integration test
//! in `tests/NAME.rs` through `siderite-testkit`.
//!
//! The tests check behavior a client relies on rather than restating the
//! handler: status codes, content type, the documented operation, a
//! create-then-fetch round trip and rejected input. Each has a failure
//! case. A resource test creates its own in-memory SQLite database from
//! the model's metadata, so no configured database is touched; route
//! tests use none.

use super::ident::pascal;
use super::model::{self, FieldSpec, ModelOptions};
use super::route::{self, RouteOptions};
use crate::error::CliError;
use std::fmt::Write;
use std::path::Path;

/// What the test covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TestOptions {
    /// A module written by `generate route`.
    Route(RouteOptions),
    /// A module written by `generate resource`.
    Resource(ModelOptions),
}

const USAGE: &str = "usage: generate test route NAME [--path /URL] \
     [--dry-run]\n       generate test resource NAME [FIELD:TYPE[?]]... \
     [--table TABLE] [--dry-run]";

/// Parse the words after `generate test`.
///
/// Errors:
///   A usage error for an unknown kind, or the kind's own errors.
pub(crate) fn parse(words: &[String]) -> Result<TestOptions, CliError> {
    let usage = || CliError::usage(USAGE);
    let (kind, rest) = words.split_first().ok_or_else(usage)?;
    match kind.as_str() {
        "route" => route::parse(rest).map(TestOptions::Route),
        "resource" => model::parse(rest).map(TestOptions::Resource),
        _ => Err(usage()),
    }
}

fn name(options: &TestOptions) -> &str {
    match options {
        TestOptions::Route(route) => &route.name,
        TestOptions::Resource(resource) => &resource.name,
    }
}

/// Where the test is written, relative to the package.
pub(crate) fn file(options: &TestOptions) -> String {
    format!("tests/{}.rs", name(options))
}

/// The test's source for the library crate `krate`.
pub(crate) fn render(options: &TestOptions, krate: &str) -> String {
    match options {
        TestOptions::Route(route) => render_route(route, krate),
        TestOptions::Resource(resource) => render_resource(resource, krate),
    }
}

fn render_route(options: &RouteOptions, krate: &str) -> String {
    let RouteOptions { name, path } = options;
    format!(
        "\
//! `GET {path}` through the in-process test client.

use siderite_testkit::TestClient;

fn client() -> TestClient {{
    TestClient::new({krate}::app())
}}

#[tokio::test]
async fn {name}_answers_with_json() {{
    let response = client().get(\"{path}\").await.unwrap();
    assert_eq!(response.status, 200, \"{{}}\", response.text());
    assert_eq!(response.content_type(), Some(\"application/json\"));
    let body: serde_json::Value = response.json().unwrap();
    assert!(body[\"message\"].is_string(), \"{{body}}\");
}}

#[tokio::test]
async fn {name}_rejects_other_methods() {{
    let response = client().delete(\"{path}\").await.unwrap();
    assert_eq!(response.status, 405);
}}

#[tokio::test]
async fn {name}_is_documented() {{
    let document = {krate}::app().openapi().unwrap();
    let document = serde_json::to_value(document).unwrap();
    let operation = &document[\"paths\"][\"{path}\"][\"get\"];
    assert_eq!(operation[\"operationId\"], \"{name}_index\");
}}
"
    )
}

/// A JSON literal of the field's type, the same on every run.
fn sample(field: &FieldSpec) -> &'static str {
    match field.ty {
        "String" => "\"example\"",
        "bool" => "true",
        "f64" => "1.5",
        _ => "7",
    }
}

/// `"name": value,` lines for `fields`.
fn json_fields(fields: &[&FieldSpec], indent: &str) -> String {
    let mut out = String::new();
    for f in fields {
        let _ = writeln!(out, "{indent}\"{}\": {},", f.name, sample(f));
    }
    out
}

/// Bodies the creation endpoint must reject: an `id` (keys never come
/// from clients) and, when there is one, a missing required field.
fn invalid_bodies(fields: &[FieldSpec]) -> String {
    let all: Vec<&FieldSpec> = fields.iter().collect();
    let indent = "            ";
    let mut out = format!(
        "        serde_json::json!({{\n{indent}\"id\": 1,\n{}        }}),\n",
        json_fields(&all, indent)
    );
    if let Some(required) = fields.iter().position(|f| !f.nullable) {
        let mut rest = all.clone();
        rest.remove(required);
        let _ = writeln!(
            out,
            "        serde_json::json!({{\n{}        }}),",
            json_fields(&rest, indent)
        );
    }
    out
}

fn render_resource(options: &ModelOptions, krate: &str) -> String {
    let ModelOptions {
        name,
        table,
        fields,
    } = options;
    let ty = pascal(name);
    let all: Vec<&FieldSpec> = fields.iter().collect();
    let body = json_fields(&all, "        ");
    let invalid = invalid_bodies(fields);
    format!(
        "\
//! `/{table}` through the in-process test client, on a disposable
//! in-memory SQLite database created from the model.

use {krate}::models::{name}::{ty};
use siderite::prelude::*;
use siderite_testkit::{{TestClient, TestDatabase}};

/// A client for the app with a fresh database as `default`; keep the
/// database alive as long as the client.
async fn client() -> (TestClient, TestDatabase) {{
    let test = TestDatabase::sqlite_memory().await.unwrap();
    let test = test.with_models(&[{ty}::META]).await.unwrap();
    let client = TestClient::builder({krate}::app())
        .with_database(\"default\", test.db().clone())
        .build();
    (client, test)
}}

#[tokio::test]
async fn {name}_round_trips() {{
    let (client, _test) = client().await;
    let body = serde_json::json!({{
{body}    }});
    let created = client.post_json(\"/{table}\", &body).await.unwrap();
    assert_eq!(created.status, 201, \"{{}}\", created.text());
    let created: serde_json::Value = created.json().unwrap();
    for (key, value) in body.as_object().unwrap() {{
        assert_eq!(&created[key], value, \"{{key}}\");
    }}
    let id = created[\"id\"].as_i64().unwrap();
    let fetched = client.get(&format!(\"/{table}/{{id}}\")).await.unwrap();
    assert_eq!(fetched.status, 200);
    assert_eq!(fetched.json::<serde_json::Value>().unwrap(), created);
    let page = client.get(\"/{table}\").await.unwrap();
    let page: serde_json::Value = page.json().unwrap();
    assert_eq!(page[\"total\"], 1);
}}

#[tokio::test]
async fn {name}_rejects_invalid_input_and_unknown_ids() {{
    let (client, test) = client().await;
    for body in [
{invalid}    ] {{
        let response = client.post_json(\"/{table}\", &body).await.unwrap();
        assert_eq!(response.status, 422, \"{{body}}\");
    }}
    let missing = client.get(\"/{table}/999999\").await.unwrap();
    assert_eq!(missing.status, 404);
    let stored = {ty}::objects(test.db()).count().await.unwrap();
    assert_eq!(stored, 0, \"rejected requests stored nothing\");
}}
"
    )
}

const EXPECTS: &str = "The test expects (siderite does not edit your code):\n";

/// What the test needs, which siderite leaves to the user: the module
/// registered in `app()`, a library target, and the dev-dependencies.
pub(crate) fn instructions(dir: &Path, options: &TestOptions) -> String {
    let manifest = std::fs::read_to_string(dir.join("Cargo.toml"));
    let manifest = manifest.unwrap_or_default();
    let mut out = String::from(EXPECTS);
    let name = name(options);
    let kind = match options {
        TestOptions::Route(_) => "route",
        TestOptions::Resource(_) => "resource",
    };
    let _ = writeln!(
        out,
        "  - the {kind} `{name}` registered in `app()` in src/lib.rs"
    );
    if !manifest.contains("siderite-testkit") {
        let _ = writeln!(
            out,
            "  - [dev-dependencies] siderite-testkit (same source as \
             siderite) and serde_json = \"1\""
        );
    }
    let _ = writeln!(out, "Then run `siderite test`.");
    out
}
