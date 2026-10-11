//! Typed reports and the output contract shared by the text and JSON
//! renderers.
//!
//! The layers are separate on purpose:
//!
//! 1. **Builders** ([`build`]) are pure: they read the app, the registered
//!    models and the settings and return typed reports. They open no
//!    database connection and print nothing.
//! 2. **Renderers** ([`text`], [`json`]) turn a report into a string. No
//!    renderer parses another renderer's output.
//!
//! Every report travels in an [`Envelope`] whose `schema_version` only
//! changes for breaking changes; new fields are additive. The JSON Schema
//! is `docs/schemas/cli-report-v1.json`.
//!
//! Exit codes are `0` (success), `1` (failure, including a `check` that
//! found an error) and `2` (usage error); see [`EXIT_OK`].

pub mod build;
pub mod json;
mod redact;
pub mod text;

#[cfg(test)]
mod tests;

pub use redact::{REDACTED, redact_field, redact_text};

use serde::{Deserialize, Serialize};

/// Version of the JSON envelope. Bumped only for breaking changes.
pub const SCHEMA_VERSION: u32 = 1;

/// Exit code of a successful command.
pub const EXIT_OK: u8 = 0;
/// Exit code of a failed command.
pub const EXIT_FAILURE: u8 = 1;
/// Exit code of a usage error.
pub const EXIT_USAGE: u8 = 2;

/// How serious a [`Diagnostic`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Informational only.
    Info,
    /// Worth fixing; the app still runs.
    Warning,
    /// The app or its migrations will not work.
    Error,
}

impl Severity {
    /// The lower-case name used in text and JSON output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// One finding. Free-text fields are passed through [`redact_text`] by
/// [`Diagnostic::new`], so a connection string never leaks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Stable identifier such as `models.E003`.
    pub code: String,
    /// Severity.
    pub severity: Severity,
    /// Human-readable description.
    pub message: String,
    /// Where the problem is (a file, model or route), if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// How to fix it, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Diagnostic {
    /// Create a diagnostic with a redacted `message`.
    pub fn new(code: &str, severity: Severity, msg: &str) -> Self {
        Self {
            code: code.to_owned(),
            severity,
            message: redact_text(msg),
            location: None,
            hint: None,
        }
    }

    /// Attach a redacted location.
    #[must_use]
    pub fn with_location(mut self, location: &str) -> Self {
        self.location = Some(redact_text(location));
        self
    }

    /// Attach a redacted hint.
    #[must_use]
    pub fn with_hint(mut self, hint: &str) -> Self {
        self.hint = Some(redact_text(hint));
        self
    }
}

/// The versioned wrapper around every structured report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<T> {
    /// Envelope version, [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The command that produced the report (`check`, `routes`, ...).
    pub command: String,
    /// Whether the command succeeded.
    pub ok: bool,
    /// The command's report.
    pub data: T,
    /// Findings, in a stable order.
    pub diagnostics: Vec<Diagnostic>,
}

impl<T> Envelope<T> {
    /// A report whose success is decided by its diagnostics: it is `ok`
    /// unless one of them is an error.
    pub fn new(command: &str, data: T, diags: Vec<Diagnostic>) -> Self {
        let ok = !diags.iter().any(|d| d.severity == Severity::Error);
        Self {
            schema_version: SCHEMA_VERSION,
            command: command.to_owned(),
            ok,
            data,
            diagnostics: diags,
        }
    }

    /// The process exit code of the command: [`EXIT_OK`] or
    /// [`EXIT_FAILURE`].
    pub const fn exit_code(&self) -> u8 {
        if self.ok { EXIT_OK } else { EXIT_FAILURE }
    }
}

/// Summary of a `check` run (the `data` of its envelope).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSummary {
    /// Number of error diagnostics.
    pub errors: usize,
    /// Number of warning diagnostics.
    pub warnings: usize,
}

/// One documented route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteReport {
    /// Upper-case HTTP method.
    pub method: String,
    /// Full path template, including mount prefixes.
    pub path: String,
    /// The operation id, if the endpoint has one.
    pub operation_id: Option<String>,
    /// OpenAPI tags, sorted.
    pub tags: Vec<String>,
    /// One-line summary, if documented.
    pub summary: Option<String>,
}

/// The `data` of a `routes` envelope. Routes are the documented routes
/// of the OpenAPI document (hidden endpoints are omitted), sorted by path
/// then method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutesData {
    /// The routes.
    pub routes: Vec<RouteReport>,
}

/// One column of a registered model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldReport {
    /// Rust field name.
    pub name: String,
    /// Column name.
    pub column: String,
    /// Column type family (`BigInt`, `Text`, ...).
    #[serde(rename = "type")]
    pub sql_type: String,
    /// Accepts `NULL`.
    pub nullable: bool,
    /// Part of the primary key.
    pub pk: bool,
}

/// One registered model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelReport {
    /// Model name.
    pub name: String,
    /// Table name.
    pub table: String,
    /// Whether migrations manage the table.
    pub managed: bool,
    /// Fields, sorted by name then column.
    pub fields: Vec<FieldReport>,
}

/// The `data` of a models report, sorted by name then table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsData {
    /// The models.
    pub models: Vec<ModelReport>,
}
