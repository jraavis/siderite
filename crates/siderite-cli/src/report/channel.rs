//! Where JSON reports go, and the envelope of a command that failed
//! before it could produce its report.
//!
//! An application binary writes its report to stdout, unless the
//! [`REPORT_PATH_ENV`] variable names a file. The `siderite` tool sets it
//! when it runs the application through `cargo run`, and sends the
//! child's stdout to stderr, so whatever the application factory prints
//! can never corrupt the JSON on stdout.

use super::{Diagnostic, Envelope, Severity, json};
use crate::error::CliError;
use serde::Serialize;
use std::io::Write;
use std::path::Path;

/// Environment variable naming the file an application binary writes its
/// JSON report to instead of stdout.
pub const REPORT_PATH_ENV: &str = "SIDERITE_REPORT_PATH";

type Done = Result<(), CliError>;

fn io_error(target: &str, err: &std::io::Error) -> CliError {
    CliError::Io(format!("cannot write {target}: {err}"))
}

/// Diagnostic code of a usage error (exit `2`).
pub const USAGE_CODE: &str = "cli.E001";
/// Diagnostic code of a command that failed without a report (exit `1`).
pub const FAILED_CODE: &str = "cli.E002";
/// Diagnostic code of an application that exited without writing its
/// report (build failure, crash, or a `siderite-cli` without `--json`).
pub const NO_REPORT_CODE: &str = "cli.E003";

/// The envelope of `command` when it failed before producing a report:
/// `ok` is `false`, `data` is `null` and one error diagnostic explains
/// why.
///
/// Args:
///   command: The command that was requested.
///   code: [`USAGE_CODE`], [`FAILED_CODE`] or [`NO_REPORT_CODE`].
///   message: What went wrong; it is redacted.
///
/// Returns:
///   The envelope to render.
pub fn failure(command: &str, code: &str, message: &str) -> Envelope<()> {
    let diag = Diagnostic::new(code, Severity::Error, message);
    Envelope::new(command, (), vec![diag])
}

/// The failure envelope of a [`CliError`].
pub fn error_envelope(command: &str, err: &CliError) -> Envelope<()> {
    let code = if err.exit_code() == 2 {
        USAGE_CODE
    } else {
        FAILED_CODE
    };
    failure(command, code, &err.to_string())
}

/// Write a rendered report to `path` (the value of [`REPORT_PATH_ENV`])
/// or, when it is `None`, to stdout.
///
/// Errors:
///   [`CliError::Io`] when the report cannot be written.
pub fn emit(rendered: &str, path: Option<&Path>) -> Result<(), CliError> {
    if let Some(path) = path {
        let written = std::fs::write(path, rendered);
        let shown = path.display().to_string();
        return written.map_err(|err| io_error(&shown, &err));
    }
    let mut out = std::io::stdout().lock();
    out.write_all(rendered.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|err| io_error("stdout", &err))
}

/// Render `report` as JSON and [`emit`] it.
///
/// Errors:
///   [`CliError::Io`] when the report cannot be serialized or written.
pub fn emit_json<T: Serialize>(report: &T, path: Option<&Path>) -> Done {
    let rendered = json::render(report);
    let rendered = rendered.map_err(|err| CliError::Io(err.to_string()))?;
    emit(&rendered, path)
}
