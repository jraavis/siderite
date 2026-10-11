//! Every resolved configuration value with the source that set it, from
//! the same merged sources [`ConfigBuilder::build`](crate::ConfigBuilder)
//! reads, so the explanation and the loaded settings cannot disagree.

use crate::builder::ALIASES;
use crate::error::is_secret_key;
use crate::redact::{REDACTED, redact_text};
use figment::Figment;
use figment::value::{Dict, Value};
use serde::Serialize;

/// The kind of source that set a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A compiled default.
    Default,
    /// A TOML file.
    File,
    /// An environment variable.
    Environment,
    /// A programmatic override (`ConfigBuilder::set`).
    Override,
}

/// Where a value came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigSource {
    /// The kind of source.
    pub kind: SourceKind,
    /// The file path or environment variable name, when known.
    pub detail: Option<String>,
}

/// One resolved value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigEntry {
    /// Dotted key, `databases.default.url`.
    pub key: String,
    /// The value, redacted: secret keys show only a URL without its
    /// credentials, or `***`; other text loses any credential it holds.
    pub value: serde_json::Value,
    /// The source with the highest precedence that set it.
    pub source: ConfigSource,
}

type Entries = Vec<ConfigEntry>;

/// The resolved entries of `merged`, sorted by key; `overrides` tells
/// programmatic overrides from defaults.
pub(crate) fn entries(merged: &Figment, overrides: &Figment) -> Entries {
    let Ok(root) = merged.extract::<Dict>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk(&root, "", &mut |key, value| {
        out.push(ConfigEntry {
            key: key.to_owned(),
            value: shown(key, value),
            source: source(merged, overrides, key),
        });
    });
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// Call `leaf` for every non-dictionary value under `dict`.
fn walk(dict: &Dict, prefix: &str, leaf: &mut dyn FnMut(&str, &Value)) {
    for (name, value) in dict {
        let key = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        match value {
            Value::Dict(_, inner) => walk(inner, &key, leaf),
            other => leaf(&key, other),
        }
    }
}

/// The value as it may be shown.
fn shown(key: &str, value: &Value) -> serde_json::Value {
    let secret = key.split('.').any(is_secret_key);
    match value {
        Value::String(_, text) if secret && text.contains("://") => {
            serde_json::Value::String(redact_text(text))
        }
        _ if secret => serde_json::Value::String(REDACTED.to_owned()),
        Value::String(_, text) => serde_json::Value::String(redact_text(text)),
        other => {
            let json = serde_json::to_value(other);
            json.unwrap_or(serde_json::Value::Null)
        }
    }
}

/// The environment variable that set `key`: the provider's prefix (shown
/// in backticks in its name) plus the key nested on `__`, or an alias.
fn variable(provider: &str, key: &str) -> Option<String> {
    let prefix = provider.split('`').nth(1).filter(|p| !p.is_empty());
    match prefix {
        Some(prefix) => {
            let nested = key.split('.').collect::<Vec<_>>().join("__");
            Some(format!("{prefix}{}", nested.to_ascii_uppercase()))
        }
        None => ALIASES
            .iter()
            .find(|(_, path)| *path == key)
            .map(|(var, _)| (*var).to_owned()),
    }
}

fn source(merged: &Figment, overrides: &Figment, key: &str) -> ConfigSource {
    if overrides.find_value(key).is_ok() {
        return ConfigSource {
            kind: SourceKind::Override,
            detail: None,
        };
    }
    let Some(meta) = merged.find_metadata(key) else {
        return ConfigSource {
            kind: SourceKind::Default,
            detail: None,
        };
    };
    if let Some(figment::Source::File(path)) = &meta.source {
        return ConfigSource {
            kind: SourceKind::File,
            detail: Some(path.display().to_string()),
        };
    }
    if meta.name.contains("environment") {
        return ConfigSource {
            kind: SourceKind::Environment,
            detail: variable(&meta.name, key),
        };
    }
    ConfigSource {
        kind: SourceKind::Default,
        detail: None,
    }
}
