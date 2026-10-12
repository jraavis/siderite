//! The tools the server offers.

use super::bridge::Bridge;
use super::limits::Limits;
use super::server::{Outcome, ToolSpec};
use serde_json::{Value, json};
use std::sync::Arc;

/// The application the server may run, and the database aliases its
/// live tools may read.
pub(crate) struct App {
    /// The selected application.
    pub bridge: Arc<Bridge>,
    /// Aliases named with `--live --database ALIAS`; empty without
    /// `--live`.
    pub live: Vec<String>,
}

/// Every tool, in the order clients list them: the project tools only
/// when an application was selected (`--app`), the live tool only for
/// aliases named with `--live`. None applies migrations or writes code.
pub(crate) fn all(app: Option<&App>, limits: Limits) -> Vec<ToolSpec> {
    let mut tools = vec![about()];
    if let Some(app) = app {
        tools.extend(project(&app.bridge, limits));
        tools.push(check(&app.bridge, limits));
        if !app.live.is_empty() {
            tools.push(status(app, limits));
        }
    }
    tools
}

/// `project_check`: the framework checks of `siderite check`.
fn check(bridge: &Arc<Bridge>, limits: Limits) -> ToolSpec {
    let input = json!({ "type": "object", "properties": {} });
    let bridge = Arc::clone(bridge);
    let run = move |_| {
        let bridge = Arc::clone(&bridge);
        async move {
            let report = bridge.report(&["--json", "check"]).await?;
            Ok(json!({
                "ok": report["ok"],
                "summary": report["data"],
                "diagnostics": report["diagnostics"],
            }))
        }
    };
    let description = "siderite's framework checks (models, migrations, \
         routes, OpenAPI, backend), as `siderite check` runs them. These \
         are not Rust lints: `siderite lint` (Clippy) is separate.";
    let tool = ToolSpec::new("project_check", description, input, run);
    tool.with_timeout(limits.build_timeout)
}

/// `migration_status`: applied and pending migrations of one of the
/// aliases the server was started with, read from the live database.
fn status(app: &App, limits: Limits) -> ToolSpec {
    let aliases = app.live.clone();
    let input = json!({
        "type": "object",
        "properties": { "database": { "type": "string", "enum": aliases } },
        "required": ["database"],
    });
    let bridge = Arc::clone(&app.bridge);
    let run = move |args: rmcp::model::JsonObject| {
        let bridge = Arc::clone(&bridge);
        let aliases = aliases.clone();
        async move {
            let alias = args.get("database").and_then(Value::as_str);
            let alias = alias.filter(|a| aliases.iter().any(|x| x == a));
            let Some(alias) = alias else {
                let allowed = aliases.join(", ");
                return Err(format!("`database` must be one of: {allowed}"));
            };
            let args = ["--database", alias, "--json", "showmigrations"];
            let report = bridge.report(&args).await?;
            Ok(json!({ "database": alias, "report": report }))
        }
    };
    let description = "Applied and pending migrations of a database alias \
         the server was started with (`--live --database`), read from the \
         live database. It never applies or rolls back migrations.";
    let tool = ToolSpec::new("migration_status", description, input, run);
    tool.with_timeout(limits.build_timeout)
}

/// What each project tool returns from the app's context.
type Pick = fn(&Value) -> Value;

const PROJECT: [(&str, &str, Pick); 4] = [
    (
        "project_summary",
        "The selected app: framework version, title, enabled features, \
         database aliases and backends (never URLs), counts and \
         completeness of the metadata.",
        summary,
    ),
    (
        "project_routes",
        "The app's documented routes: method, path, operation ID, tags.",
        |c| c["snapshot"]["routes"].clone(),
    ),
    (
        "project_models",
        "The app's registered models: table, fields, keys, relations.",
        |c| c["snapshot"]["models"].clone(),
    ),
    (
        "project_openapi",
        "The app's OpenAPI document, as `siderite openapi export` writes \
         it (null when it was too large to include).",
        |c| c["snapshot"]["openapi"].clone(),
    ),
];

fn summary(context: &Value) -> Value {
    let snapshot = &context["snapshot"];
    let count = |key: &str| snapshot[key].as_array().map_or(0, Vec::len);
    json!({
        "framework": snapshot["framework"],
        "app": snapshot["app"],
        "features": snapshot["features"],
        "databases": snapshot["databases"],
        "models": count("models"),
        "routes": count("routes"),
        "completeness": snapshot["completeness"],
        "migrations": context["migrations"],
        "binary": context["source"],
    })
}

/// The project tools over `bridge`: each builds the app if needed.
fn project(bridge: &Arc<Bridge>, limits: Limits) -> Vec<ToolSpec> {
    let input = json!({ "type": "object", "properties": {} });
    let tools = PROJECT.iter().map(|(name, description, pick)| {
        let bridge = Arc::clone(bridge);
        let pick = *pick;
        let run = move |_| {
            let bridge = Arc::clone(&bridge);
            async move { answer(&bridge, pick).await }
        };
        let tool = ToolSpec::new(name, description, input.clone(), run);
        tool.with_timeout(limits.build_timeout)
    });
    tools.collect()
}

async fn answer(bridge: &Bridge, pick: Pick) -> Outcome {
    let (context, freshness) = bridge.context().await?;
    Ok(json!({ "snapshot": freshness, "data": pick(&context) }))
}

/// `about`: the server's version, limits and tools.
fn about() -> ToolSpec {
    let input = json!({ "type": "object", "properties": {} });
    ToolSpec::new(
        "about",
        "The siderite version this server describes and its limits.",
        input,
        |_| async {
            let limits = super::limits::Limits::default();
            Ok(json!({
                "siderite": env!("CARGO_PKG_VERSION"),
                "read_only": true,
                "limits": {
                    "max_message_bytes": limits.max_message,
                    "call_timeout_secs": limits.call_timeout.as_secs(),
                    "max_result_bytes": limits.max_result,
                },
            }))
        },
    )
}
