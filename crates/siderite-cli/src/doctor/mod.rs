//! `doctor [--json]`: offline diagnostics of the toolchain, the project,
//! its configuration and the local environment.
//!
//! Nothing here connects to a database or builds the project. Native
//! database clients are reported for information only (only `dbshell`
//! needs them), and the listen-address check is advisory: the port can be
//! taken or freed between this check and `run`.

mod checks;
mod tools;

#[cfg(test)]
mod tests;

use crate::error::CliError;
use crate::report::{Diagnostic, Envelope, Severity, channel, redact_text};
use crate::target::ProjectArgs;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::PathBuf;

/// Outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    /// Fine.
    Ok,
    /// Information only; never a failure.
    Info,
    /// Worth fixing; see the matching diagnostic.
    Warning,
    /// Blocks development; see the matching diagnostic.
    Error,
}

/// One line of the doctor report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DoctorCheck {
    /// Stable check name such as `toolchain.rustc`.
    pub name: String,
    /// Outcome.
    pub status: Status,
    /// What was found (redacted).
    pub detail: String,
}

/// The `data` of a `doctor` envelope, in check order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DoctorData {
    /// Every check that ran.
    pub checks: Vec<DoctorCheck>,
}

/// What the checks look at; tests supply their own.
#[derive(Debug, Clone)]
pub(crate) struct Probe {
    /// `PATH` used to find `rustc`, `cargo` and database clients.
    pub path: OsString,
    /// Directory `siderite` runs in.
    pub cwd: PathBuf,
    /// `--manifest-path` / `--package` / `--bin`.
    pub project: ProjectArgs,
    /// Whether `SIDERITE_*`, `DATABASE_URL` and `ADDR` are read from the
    /// process environment.
    pub process_env: bool,
}

/// `(detail, hint)` of a flagged check.
type Text<'a> = (&'a str, &'a str);

/// Checks and diagnostics collected in order.
#[derive(Debug, Default)]
pub(crate) struct Findings {
    checks: Vec<DoctorCheck>,
    diagnostics: Vec<Diagnostic>,
}

impl Findings {
    fn push(&mut self, name: &str, status: Status, detail: &str) {
        self.checks.push(DoctorCheck {
            name: name.to_owned(),
            status,
            detail: redact_text(detail),
        });
    }

    pub(crate) fn ok(&mut self, name: &str, detail: &str) {
        self.push(name, Status::Ok, detail);
    }

    pub(crate) fn info(&mut self, name: &str, detail: &str) {
        self.push(name, Status::Info, detail);
    }

    /// A warning check with diagnostic `code` and an actionable `hint`.
    pub(crate) fn warn(&mut self, name: &str, code: &str, text: Text) {
        self.push(name, Status::Warning, text.0);
        self.diagnose(Severity::Warning, name, code, text);
    }

    /// An error check with diagnostic `code` and an actionable `hint`.
    pub(crate) fn error(&mut self, name: &str, code: &str, text: Text) {
        self.push(name, Status::Error, text.0);
        self.diagnose(Severity::Error, name, code, text);
    }

    fn diagnose(&mut self, s: Severity, name: &str, code: &str, text: Text) {
        let (detail, hint) = text;
        let diag = Diagnostic::new(code, s, detail)
            .with_location(name)
            .with_hint(hint);
        self.diagnostics.push(diag);
    }

    fn into_envelope(self) -> Envelope<DoctorData> {
        let data = DoctorData {
            checks: self.checks,
        };
        Envelope::new("doctor", data, self.diagnostics)
    }
}

/// Run every check.
pub(crate) fn diagnose(probe: &Probe) -> Envelope<DoctorData> {
    let mut findings = Findings::default();
    checks::run_all(probe, &mut findings);
    findings.into_envelope()
}

/// `siderite doctor [--json]` from the process environment.
///
/// Errors:
///   A usage error for extra arguments, or an IO error writing output.
pub(crate) fn run(
    rest: &[String],
    cwd: PathBuf,
    project: ProjectArgs,
    json: bool,
) -> Result<u8, CliError> {
    if let Some(extra) = rest.iter().skip(1).find(|a| *a != "--json") {
        let msg = format!("unexpected argument `{extra}` for `doctor`");
        return Err(CliError::usage(msg));
    }
    let probe = Probe {
        path: std::env::var_os("PATH").unwrap_or_default(),
        cwd,
        project,
        process_env: true,
    };
    let report = diagnose(&probe);
    if json {
        channel::emit_json(&report, None)?;
    } else {
        print!("{}", render(&report));
    }
    Ok(report.exit_code())
}

/// The text form: one aligned line per check, hints under flagged ones,
/// then a summary line.
pub(crate) fn render(report: &Envelope<DoctorData>) -> String {
    let mut out = String::new();
    let mut hints = report.diagnostics.iter();
    for check in &report.data.checks {
        let status = match check.status {
            Status::Ok => "ok",
            Status::Info => "info",
            Status::Warning => "warning",
            Status::Error => "error",
        };
        out.push_str(&format!(
            "{status:<8} {:<20} {}\n",
            check.name, check.detail
        ));
        if matches!(check.status, Status::Warning | Status::Error)
            && let Some(hint) = hints.next().and_then(Diagnostic::hint)
        {
            out.push_str(&format!("{:<29} hint: {hint}\n", ""));
        }
    }
    let count = |s: Severity| {
        let diags = report.diagnostics.iter();
        diags.filter(|d| d.severity() == s).count()
    };
    out.push_str(&format!(
        "Doctor found {} error(s) and {} warning(s).\n",
        count(Severity::Error),
        count(Severity::Warning)
    ));
    out
}
