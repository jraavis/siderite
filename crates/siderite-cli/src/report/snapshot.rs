//! A bounded, versioned snapshot of an application's metadata: the
//! framework version, compiled features, database aliases with their
//! backend kinds, registered models, documented routes and the OpenAPI
//! document.
//!
//! The snapshot is built from compiled metadata only (no connection is
//! opened) and is safe to hand to tools: database URLs are never included
//! (only alias names and backend kinds), every string of the OpenAPI
//! document and the app description is redacted, and no row data exists
//! to leak. Each section says whether it is complete; anything left out
//! to respect [`Limits`] is reported, never silently dropped.

use super::build::{model_reports, route_reports};
use super::redact_text;
use super::{Diagnostic, Envelope, ModelReport, RouteReport, Severity};
use crate::connect::backend_kind;
use crate::settings::CliSettings;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use siderite_core::App;
use siderite_orm::ModelMeta;

/// The `format` of a snapshot.
pub const FORMAT: &str = "siderite-snapshot";
/// Bumped only for breaking changes of the snapshot shape.
pub const FORMAT_VERSION: u32 = 1;

/// Size bounds of a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most bytes of the serialized snapshot.
    pub max_bytes: usize,
    /// Most models.
    pub max_models: usize,
    /// Most routes.
    pub max_routes: usize,
    /// Most bytes of the embedded OpenAPI document.
    pub max_openapi_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 1 << 20,
            max_models: 500,
            max_routes: 2_000,
            max_openapi_bytes: 512 << 10,
        }
    }
}

/// The framework that produced the snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Framework {
    /// `siderite`.
    pub name: String,
    /// Version of the framework compiled into the app.
    pub version: String,
}

/// The app's OpenAPI metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    /// API title.
    pub title: String,
    /// API version.
    pub version: String,
    /// API description (redacted).
    pub description: Option<String>,
}

/// A configured database alias. The URL is never included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseInfo {
    /// Alias name.
    pub alias: String,
    /// `sqlite`, `postgres`, `mysql`, `mongodb`, `redis` or `unknown`.
    pub backend: String,
}

/// Whether a section holds everything, and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    /// Nothing was left out.
    pub complete: bool,
    /// How many items exist, when known.
    pub total: Option<usize>,
    /// How many are included.
    pub included: usize,
    /// Why the section is incomplete.
    pub reason: Option<String>,
}

impl Section {
    fn full(count: usize) -> Self {
        Self {
            complete: true,
            total: Some(count),
            included: count,
            reason: None,
        }
    }

    fn missing(reason: &str) -> Self {
        Self {
            complete: false,
            total: None,
            included: 0,
            reason: Some(reason.to_owned()),
        }
    }
}

/// Completeness of each section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completeness {
    /// The `models` section.
    pub models: Section,
    /// The `routes` section.
    pub routes: Section,
    /// The `openapi` section (`included` is 1 or 0).
    pub openapi: Section,
}

/// The snapshot (the `data` of a `snapshot` envelope).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Always [`FORMAT`].
    pub format: String,
    /// [`FORMAT_VERSION`].
    pub format_version: u32,
    /// The framework.
    pub framework: Framework,
    /// The app.
    pub app: AppInfo,
    /// `siderite-cli` features compiled into the app binary.
    pub features: Vec<String>,
    /// Configured aliases, sorted by name.
    pub databases: Vec<DatabaseInfo>,
    /// Registered models, sorted by name then table.
    pub models: Vec<ModelReport>,
    /// Documented routes, sorted by path then method.
    pub routes: Vec<RouteReport>,
    /// The OpenAPI document, redacted, when it fits.
    pub openapi: Option<Value>,
    /// What each section holds.
    pub completeness: Completeness,
}

/// Build the snapshot of `app`, its registered `models` and `settings`
/// within `limits`.
///
/// Args:
///   app: A freshly built app (no connection is opened).
///   models: The registered model metadata.
///   settings: The CLI settings; only alias names and URL schemes are
///     read.
///   limits: Size bounds.
///
/// Returns:
///   The `snapshot` envelope. A truncated section adds a
///   `snapshot.W001` warning; an OpenAPI document that cannot be
///   generated adds a `snapshot.E001` error and leaves routes and spec
///   empty and marked incomplete.
pub fn snapshot(
    app: &App,
    models: &[&'static ModelMeta],
    settings: &CliSettings,
    limits: Limits,
) -> Envelope<Snapshot> {
    let mut diags = Vec::new();
    let mut models = model_reports(models);
    let max = limits.max_models;
    let models_section = cap(&mut models, max, "models", &mut diags);
    let documented = documented(app, limits, &mut diags);
    let (routes, openapi, routes_section, openapi_section) = documented;
    let meta = app.meta();
    let mut snapshot = Snapshot {
        format: FORMAT.to_owned(),
        format_version: FORMAT_VERSION,
        framework: Framework {
            name: "siderite".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        app: AppInfo {
            title: redact_text(&meta.title),
            version: redact_text(&meta.version),
            description: meta.description.as_deref().map(redact_text),
        },
        features: features(),
        databases: databases(settings),
        models,
        routes,
        openapi,
        completeness: Completeness {
            models: models_section,
            routes: routes_section,
            openapi: openapi_section,
        },
    };
    fit(&mut snapshot, limits.max_bytes, &mut diags);
    Envelope::new("snapshot", snapshot, diags)
}

type Documented = (Vec<RouteReport>, Option<Value>, Section, Section);
type Diags<'a> = &'a mut Vec<Diagnostic>;

/// Routes and the redacted OpenAPI document, each within its limit.
fn documented(app: &App, limits: Limits, diags: Diags<'_>) -> Documented {
    let (mut routes, document) = match (route_reports(app), app.openapi()) {
        (Ok(routes), Ok(document)) => (routes, document.to_value()),
        (Err(err), _) => return unavailable(&err.to_string(), diags),
        (_, Err(err)) => return unavailable(&err.to_string(), diags),
    };
    let routes_section = cap(&mut routes, limits.max_routes, "routes", diags);
    let document = redact_value(document);
    let size = serde_json::to_vec(&document).map_or(usize::MAX, |b| b.len());
    if size > limits.max_openapi_bytes {
        let reason = format!(
            "the OpenAPI document has {size} bytes, over the limit of {}",
            limits.max_openapi_bytes
        );
        diags.push(truncated(&reason));
        let mut section = Section::missing(&reason);
        section.total = Some(1);
        return (routes, None, routes_section, section);
    }
    (routes, Some(document), routes_section, Section::full(1))
}

fn unavailable(message: &str, diags: &mut Vec<Diagnostic>) -> Documented {
    let reason = "the OpenAPI document cannot be generated";
    let diag = Diagnostic::new("snapshot.E001", Severity::Error, message)
        .with_hint("run `check` for the duplicate or invalid route");
    diags.push(diag);
    let section = Section::missing(reason);
    (Vec::new(), None, section.clone(), section)
}

/// Keep the first `max` items (they are sorted), reporting a cut.
fn cap<T>(items: &mut Vec<T>, max: usize, what: &str, d: Diags) -> Section {
    let total = items.len();
    if total <= max {
        return Section::full(total);
    }
    items.truncate(max);
    let reason = format!("{total} {what}, over the limit of {max}");
    d.push(truncated(&reason));
    Section {
        complete: false,
        total: Some(total),
        included: max,
        reason: Some(reason),
    }
}

fn truncated(reason: &str) -> Diagnostic {
    Diagnostic::new("snapshot.W001", Severity::Warning, reason)
        .with_hint("the snapshot is partial; see `completeness`")
}

/// Shrink to `max_bytes`: drop the OpenAPI document first, then routes
/// and models from the end, so the result is deterministic. Stops when
/// it fits or nothing is left to drop.
fn fit(snapshot: &mut Snapshot, max_bytes: usize, diags: Diags<'_>) {
    let reason = format!("the snapshot is over {max_bytes} bytes");
    let mut reported = false;
    loop {
        let over = json_len(snapshot).saturating_sub(max_bytes);
        if over == 0 {
            return;
        }
        if !reported {
            diags.push(truncated(&reason));
            reported = true;
        }
        if snapshot.openapi.take().is_some() {
            snapshot.completeness.openapi = Section::missing(&reason);
        } else if !snapshot.routes.is_empty() {
            let left = drop_bytes(&mut snapshot.routes, over);
            mark(&mut snapshot.completeness.routes, left, &reason);
        } else if !snapshot.models.is_empty() {
            let left = drop_bytes(&mut snapshot.models, over);
            mark(&mut snapshot.completeness.models, left, &reason);
        } else {
            return;
        }
    }
}

/// Pop items until about `bytes` are gone; the items left.
fn drop_bytes<T: Serialize>(items: &mut Vec<T>, bytes: usize) -> usize {
    let mut freed = 0;
    while freed < bytes {
        let Some(item) = items.pop() else { break };
        // The item and the comma before it.
        freed += json_len(&item) + 1;
    }
    items.len()
}

fn json_len<T: Serialize>(item: &T) -> usize {
    serde_json::to_vec(item).map_or(0, |b| b.len())
}

fn mark(section: &mut Section, included: usize, reason: &str) {
    section.complete = false;
    section.included = included;
    section.reason = Some(reason.to_owned());
}

/// Every string in `value`, redacted.
fn redact_value(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_text(&text)),
        Value::Array(items) => {
            let items = items.into_iter().map(redact_value);
            Value::Array(items.collect())
        }
        Value::Object(map) => {
            let map = map.into_iter().map(|(k, v)| (k, redact_value(v)));
            Value::Object(map.collect())
        }
        other => other,
    }
}

/// `siderite-cli` features compiled into this binary.
fn features() -> Vec<String> {
    let all = [
        ("mysql", cfg!(feature = "mysql")),
        ("postgres", cfg!(feature = "postgres")),
    ];
    all.iter()
        .filter(|(_, on)| *on)
        .map(|(f, _)| (*f).to_owned())
        .collect()
}

/// Alias names and backend kinds; URLs never leave this function.
fn databases(settings: &CliSettings) -> Vec<DatabaseInfo> {
    settings
        .database_urls()
        .iter()
        .map(|(alias, url)| DatabaseInfo {
            alias: alias.clone(),
            backend: backend_kind(url).map_or_else(
                || "unknown".to_owned(),
                |kind| format!("{kind:?}").to_ascii_lowercase(),
            ),
        })
        .collect()
}
