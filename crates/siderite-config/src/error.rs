//! Configuration errors. Messages never include secret values.

use crate::secret::REDACTED;
use figment::error::Kind;
use std::path::PathBuf;
use thiserror::Error;

/// Failure while loading settings or installing tracing.
///
/// [`std::fmt::Display`] and [`std::fmt::Debug`] never include values of secret fields (`url`,
/// `secret_key`, passwords, tokens, and similar keys).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// Figment could not merge or extract configuration.
    #[error("{0}")]
    Extract(String),
    /// A file passed to [`crate::ConfigBuilder::file`] does not exist.
    #[error("required configuration file not found: {}", .0.display())]
    MissingFile(PathBuf),
    /// `log.level` or `RUST_LOG` is not a valid tracing filter.
    #[error("invalid tracing filter: {0}")]
    InvalidFilter(String),
    /// Installing the global tracing subscriber failed.
    #[error("tracing subscriber could not be initialized: {0}")]
    Tracing(String),
}

impl From<figment::Error> for ConfigError {
    fn from(err: figment::Error) -> Self {
        let message = err
            .into_iter()
            .map(format_figment_error)
            .collect::<Vec<_>>()
            .join("; ");
        Self::Extract(message)
    }
}

fn format_figment_error(err: figment::Error) -> String {
    let path = err.path.join(".");
    let kind = format_kind(&err.kind, path_is_secret(&err.path));
    if path.is_empty() {
        kind
    } else {
        format!("{kind} at `{path}`")
    }
}

fn path_is_secret(path: &[String]) -> bool {
    path.iter().any(|segment| is_secret_key(segment))
}

/// Whether a config key may hold a secret. Errs toward redacting: a false
/// positive only hides a value in an error message, a false negative leaks it.
fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace('-', "_");
    const CONTAINS: [&str; 9] = [
        "secret",
        "passw",
        "passphrase",
        "token",
        "credential",
        "authorization",
        "cookie",
        "apikey",
        "privatekey",
    ];
    CONTAINS.iter().any(|needle| key.contains(needle))
        || key == "url"
        || key.ends_with("_url")
        || key.ends_with("_uri")
        || key == "pass"
        || key.ends_with("_pass")
        || key == "dsn"
        || key.ends_with("_dsn")
        || key == "key"
        || key.ends_with("_key")
}

fn format_kind(kind: &Kind, redact: bool) -> String {
    if !redact {
        return strip_source_excerpt(&kind.to_string());
    }
    match kind {
        Kind::InvalidType(_, expected) => {
            format!("invalid type: expected {expected}, value {REDACTED}")
        }
        Kind::InvalidValue(_, expected) => {
            format!("invalid value: expected {expected}, value {REDACTED}")
        }
        Kind::Message(_) => format!("invalid value {REDACTED}"),
        Kind::Unsupported(_) => format!("unsupported value {REDACTED}"),
        Kind::UnsupportedKey(_, expected) => {
            format!("unsupported key, expected {expected}")
        }
        Kind::InvalidLength(len, expected) => {
            format!("invalid length {len}, expected {expected}")
        }
        Kind::UnknownVariant(_, expected) => {
            format!("unknown variant, expected one of {expected:?}")
        }
        Kind::UnknownField(field, expected) => {
            format!("unknown field `{field}`, expected one of {expected:?}")
        }
        Kind::MissingField(field) => format!("missing field `{field}`"),
        Kind::DuplicateField(field) => format!("duplicate field `{field}`"),
        Kind::ISizeOutOfRange(_) | Kind::USizeOutOfRange(_) => {
            format!("integer out of range, value {REDACTED}")
        }
    }
}

/// Drop the quoted source lines of a parse error (`2 | url = "..."`, `  |  ^`).
///
/// TOML syntax errors carry no key path, so the excerpt could show a secret.
/// The location (`line 2, column 37`) and the reason are kept.
fn strip_source_excerpt(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with('|')
                && !line.split_once('|').is_some_and(|(n, _)| {
                    !n.trim().is_empty() && n.trim().bytes().all(|b| b.is_ascii_digit())
                })
        })
        .collect::<Vec<_>>()
        .join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use figment::error::Actual;

    #[test]
    fn secret_paths_are_detected() {
        assert!(path_is_secret(&[
            "databases".into(),
            "default".into(),
            "url".into()
        ]));
        assert!(path_is_secret(&["secret_key".into()]));
        assert!(path_is_secret(&["cache".into(), "url".into()]));
        assert!(!path_is_secret(&["server".into(), "addr".into()]));
        assert!(!path_is_secret(&["app".into(), "name".into()]));
    }

    #[test]
    fn secret_key_variants_are_detected() {
        for key in [
            "database_url",
            "DATABASE_URL",
            "redis_uri",
            "access_token",
            "api_token",
            "refresh_tokens",
            "private-key",
            "signing_key",
            "passphrase",
            "db_password",
            "client_secret",
            "secrets",
            "sentry_dsn",
            "pass",
            "smtp_pass",
        ] {
            assert!(is_secret_key(key), "{key} should be secret");
        }
        for key in [
            "addr",
            "port",
            "name",
            "level",
            "workers",
            "bypass",
            "passthrough",
        ] {
            assert!(!is_secret_key(key), "{key} should not be secret");
        }
    }

    #[test]
    fn toml_syntax_excerpts_are_dropped() {
        let message = "TOML parse error at line 2, column 37\n  |\n2 | url = \"postgres://u:hunter2@db/x\n  |                                     ^\ninvalid basic string\n";
        assert_eq!(
            strip_source_excerpt(message),
            "TOML parse error at line 2, column 37: invalid basic string"
        );
        assert_eq!(strip_source_excerpt("a | b"), "a | b");
    }

    #[test]
    fn toml_syntax_error_from_file_omits_secret() {
        let dir =
            std::env::temp_dir().join(format!("siderite-config-syntax-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_default();
        let file = dir.join("siderite.toml");
        std::fs::write(
            &file,
            "[databases.default]\nurl = \"postgres://u:hunter2@db/x\n",
        )
        .unwrap_or_default();
        let err = crate::ConfigBuilder::new().file(&file).build();
        let rendered = err.map(|_| String::new()).unwrap_or_else(|e| e.to_string());
        assert!(rendered.contains("line 2"), "{rendered}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
    }

    #[test]
    fn redacted_out_of_range_omits_value() {
        let rendered = format_kind(&Kind::ISizeOutOfRange(-987_654_321), true);
        assert!(rendered.contains(REDACTED));
        assert!(!rendered.contains("987654321"));
    }

    #[test]
    fn redacted_kind_omits_string_payload() {
        let kind = Kind::InvalidType(
            Actual::Str("postgres://user:hunter2@db/app".into()),
            "u32".into(),
        );
        let rendered = format_kind(&kind, true);
        assert!(rendered.contains(REDACTED));
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.contains("postgres://"));
    }
}
