//! What changes for each siderite dependency, from its source kind.

use serde::Serialize;
use serde_json::Value;

/// Crates of the framework, upgraded together.
const FRAMEWORK: [&str; 3] = ["siderite", "siderite-cli", "siderite-testkit"];

/// Where a dependency comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SourceKind {
    /// A registry version requirement.
    Registry,
    /// A git repository.
    Git,
    /// A local path.
    Path,
}

/// One dependency's change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DependencyChange {
    /// The crate.
    pub name: String,
    /// Where it comes from.
    pub source: SourceKind,
    /// Whether the package inherits it from the workspace root.
    pub inherited: bool,
    /// The manifest to edit: the package's or the workspace root's.
    pub manifest: String,
    /// What it is now.
    pub current: String,
    /// What it would become; `None` when siderite cannot say.
    pub proposed: Option<String>,
}

/// Whether `manifest` (the package's `Cargo.toml` text) inherits `name`
/// from the workspace (`name.workspace = true` or `{ workspace = true }`).
pub(crate) fn inherited(manifest: &str, name: &str) -> bool {
    manifest.lines().map(str::trim).any(|line| {
        let rest = line.strip_prefix(name);
        let starts = rest.is_some_and(|r| r.starts_with(['.', ' ', '=']));
        let compact: String = line.split_whitespace().collect();
        starts && compact.contains("workspace=true")
    })
}

fn git_reference(source: &str) -> String {
    let rest = source.trim_start_matches("git+");
    let (url, query) = rest.split_once('?').unwrap_or((rest, ""));
    let url = url.split('#').next().unwrap_or(url);
    let query = query.split('#').next().unwrap_or(query);
    if query.is_empty() {
        format!("git {url} (default branch)")
    } else {
        format!("git {url} ({query})")
    }
}

type Change = Option<DependencyChange>;

/// The change for one dependency `spec` (as `cargo metadata` lists it)
/// to version `to`.
pub(crate) fn change(spec: &Value, to: &str, m: Manifests<'_>) -> Change {
    let name = spec.get("name")?.as_str()?;
    if !FRAMEWORK.contains(&name) {
        return None;
    }
    let inherited = inherited(m.text, name);
    let manifest = if inherited { m.root } else { m.package };
    let path = spec.get("path").and_then(Value::as_str);
    let source = spec.get("source").and_then(Value::as_str).unwrap_or("");
    let req = spec.get("req").and_then(Value::as_str).unwrap_or("*");
    let (kind, current, proposed) = if let Some(path) = path {
        let now = format!("path {path}");
        (SourceKind::Path, now, None)
    } else if source.starts_with("git+") {
        let now = git_reference(source);
        (SourceKind::Git, now, Some(format!("tag = \"v{to}\"")))
    } else {
        let now = format!("version \"{req}\"");
        (SourceKind::Registry, now, Some(format!("version \"{to}\"")))
    };
    Some(DependencyChange {
        name: name.to_owned(),
        source: kind,
        inherited,
        manifest: manifest.to_owned(),
        current,
        proposed,
    })
}

/// The manifests a change may be made in.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Manifests<'a> {
    /// The package's `Cargo.toml` text.
    pub text: &'a str,
    /// The package's `Cargo.toml` path, for display.
    pub package: &'a str,
    /// The workspace root `Cargo.toml` path, for display.
    pub root: &'a str,
}
