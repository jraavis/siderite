//! `check`: validate the app, its models and its configuration without
//! starting a server or opening a database.
//!
//! [`check`] returns every [`CheckIssue`] it finds. The checks, by id prefix:
//!
//! | Prefix | Checks |
//! |---|---|
//! | `config` | a `default` database exists when models are managed; database URLs parse and name a known scheme |
//! | `models` | duplicate tables, models and columns; a primary key; foreign-key and many-to-many targets are registered models |
//! | `migrations` | migration files load; the dependency graph is sound; model changes not yet in a migration |
//! | `openapi` | the OpenAPI document generates (duplicate operations and ids) |
//! | `backend` | the models fit the capabilities of the `default` database's backend |
//!
//! Issue messages never contain database URLs.

mod backend;
mod config;
mod migrations;
mod models;
mod openapi;

use crate::settings::CliSettings;
use serde::Serialize;
use siderite_core::App;
use siderite_orm::ModelMeta;
use std::fmt;
use std::path::Path;

/// How serious a [`CheckIssue`] is. Serializes as `"warning"` / `"error"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckLevel {
    /// Worth fixing; the app still runs.
    Warning,
    /// The app or its migrations will not work.
    Error,
}

impl fmt::Display for CheckLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Warning => "warning",
            Self::Error => "error",
        })
    }
}

/// One problem found by [`check`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckIssue {
    /// Severity.
    pub level: CheckLevel,
    /// Stable identifier such as `models.E001` (`E` errors, `W` warnings).
    pub id: &'static str,
    /// Human-readable description. Never contains a database URL.
    pub message: String,
}

impl CheckIssue {
    /// An error issue.
    pub fn error(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Error,
            id,
            message: message.into(),
        }
    }

    /// A warning issue.
    pub fn warning(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            level: CheckLevel::Warning,
            id,
            message: message.into(),
        }
    }
}

impl fmt::Display for CheckIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: [{}] {}", self.level, self.id, self.message)
    }
}

/// The `data` of `check --json`: every issue plus per-level counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckReport {
    /// Issues in [`check`] order.
    pub issues: Vec<CheckIssue>,
    /// Number of error issues.
    pub errors: usize,
    /// Number of warning issues.
    pub warnings: usize,
}

impl CheckReport {
    /// Count the levels of `issues`.
    #[must_use]
    pub fn new(issues: Vec<CheckIssue>) -> Self {
        let errors = issues
            .iter()
            .filter(|i| i.level == CheckLevel::Error)
            .count();
        Self {
            warnings: issues.len() - errors,
            errors,
            issues,
        }
    }
}

/// Whether any of `issues` is an error.
pub fn has_errors(issues: &[CheckIssue]) -> bool {
    issues.iter().any(|i| i.level == CheckLevel::Error)
}

/// Run every check.
///
/// `migrations_dir` is where the JSON migrations live; `None` skips the
/// migration checks. Issues come out in a stable order: config, models,
/// migrations, OpenAPI, backend.
pub fn check(
    app: &App,
    models: &[&'static ModelMeta],
    settings: &CliSettings,
    migrations_dir: Option<&Path>,
) -> Vec<CheckIssue> {
    let mut issues = Vec::new();
    issues.extend(config::check(models, settings));
    issues.extend(models::check(models));
    if let Some(dir) = migrations_dir {
        issues.extend(migrations::check(models, dir));
    }
    issues.extend(openapi::check(app));
    issues.extend(backend::check(models, settings));
    issues
}

/// Check that `app` can be turned into a router, which catches duplicate
/// routes and malformed path templates that the OpenAPI check does not see
/// (for example hidden endpoints). Consumes the app: call it on a fresh one.
pub fn check_build(app: App) -> Vec<CheckIssue> {
    openapi::check_build(app)
}

/// Whether a managed model exists, i.e. whether any table is expected.
pub(crate) fn has_managed_models(models: &[&'static ModelMeta]) -> bool {
    models.iter().any(|m| m.managed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_display_level_id_and_message() {
        let issue = CheckIssue::error("models.E001", "boom");
        assert_eq!(issue.to_string(), "error: [models.E001] boom");
        let issue = CheckIssue::warning("migrations.W001", "meh");
        assert_eq!(issue.to_string(), "warning: [migrations.W001] meh");
    }

    #[test]
    fn has_errors_ignores_warnings() {
        let warn = CheckIssue::warning("x.W001", "w");
        assert!(!has_errors(std::slice::from_ref(&warn)));
        assert!(has_errors(&[warn, CheckIssue::error("x.E001", "e")]));
        assert!(CheckLevel::Warning < CheckLevel::Error);
    }
}
