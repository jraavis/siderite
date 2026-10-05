//! Structured JSON envelope for CLI machine-readable output.

use serde::{Deserialize, Serialize};

/// Standard envelope schema version.
pub const ENVELOPE_SCHEMA_VERSION: &str = "1.0";

/// Diagnostic severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    /// Informational note.
    Info,
    /// Non-fatal warning.
    Warning,
    /// Fatal error.
    Error,
}

/// A structured diagnostic message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliDiagnostic {
    /// Machine-readable code, e.g. "USAGE", "PROJECT_NOT_FOUND".
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
    /// Severity level.
    pub severity: DiagnosticSeverity,
}

impl CliDiagnostic {
    /// Create an error diagnostic.
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            severity: DiagnosticSeverity::Error,
        }
    }

    /// Create a warning diagnostic.
    pub fn warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            severity: DiagnosticSeverity::Warning,
        }
    }

    /// Create an informational diagnostic.
    pub fn info(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            severity: DiagnosticSeverity::Info,
        }
    }
}

/// Standard envelope for all JSON responses from the CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliEnvelope<T> {
    /// Envelope format version.
    pub schema_version: String,
    /// Name of the command producing the response.
    pub command: String,
    /// Whether the command succeeded.
    pub ok: bool,
    /// Payload data, present on success.
    pub data: Option<T>,
    /// Structured diagnostics, warnings, and errors.
    pub diagnostics: Vec<CliDiagnostic>,
}

impl<T> CliEnvelope<T> {
    /// Create a successful response envelope.
    pub fn success(command: impl Into<String>, data: T) -> Self {
        Self {
            schema_version: ENVELOPE_SCHEMA_VERSION.to_owned(),
            command: command.into(),
            ok: true,
            data: Some(data),
            diagnostics: Vec::new(),
        }
    }

    /// Create a failed response envelope with diagnostics.
    pub fn failure(command: impl Into<String>, diagnostics: Vec<CliDiagnostic>) -> Self {
        Self {
            schema_version: ENVELOPE_SCHEMA_VERSION.to_owned(),
            command: command.into(),
            ok: false,
            data: None,
            diagnostics,
        }
    }

    /// Create a failed response with a single error.
    pub fn error(
        command: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::failure(command, vec![CliDiagnostic::error(code, message)])
    }
}

impl<T: Serialize> CliEnvelope<T> {
    /// Render the envelope as pretty JSON.
    ///
    /// # Errors
    /// Returns serialization error if payload serialization fails.
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Render the envelope as compact JSON.
    ///
    /// # Errors
    /// Returns serialization error if payload serialization fails.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_success_envelope() {
        let env = CliEnvelope::success("test", vec!["item1", "item2"]);
        assert!(env.ok);
        assert_eq!(env.schema_version, ENVELOPE_SCHEMA_VERSION);
        assert_eq!(env.command, "test");
        assert_eq!(env.data, Some(vec!["item1", "item2"]));
        assert!(env.diagnostics.is_empty());

        let json = env.to_json().unwrap();
        assert!(json.contains("\"schema_version\":\"1.0\""));
        assert!(json.contains("\"ok\":true"));
        assert!(json.contains("\"command\":\"test\""));
    }

    #[test]
    fn serializes_failure_envelope() {
        let env: CliEnvelope<()> = CliEnvelope::error("run", "ERR_CODE", "msg");
        assert!(!env.ok);
        assert_eq!(env.data, None);
        assert_eq!(env.diagnostics.len(), 1);
        assert_eq!(env.diagnostics[0].code, "ERR_CODE");
        assert_eq!(env.diagnostics[0].severity, DiagnosticSeverity::Error);

        let json = env.to_json().unwrap();
        assert!(json.contains("\"ok\":false"));
        assert!(json.contains("\"code\":\"ERR_CODE\""));
    }
}
