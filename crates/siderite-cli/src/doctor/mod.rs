//! `doctor [--json]`: offline diagnostics of the toolchain, the project,
//! its configuration and the local environment.
//!
//! By default nothing connects to a database or builds the project;
//! `--live` adds explicit, read-only database probes (see [`live`]). Native
//! database clients are reported for information only (only `dbshell`
//! needs them), and the listen-address check is advisory: the port can be
//! taken or freed between this check and `run`.

mod checks;
mod live;
mod tools;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_live;

use crate::error::CliError;
use crate::report::{Diagnostic, Envelope, Severity, channel, redact_text};
use crate::target::ProjectArgs;
use live::Live;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

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

/// The doctor envelope.
pub(crate) type Report = Envelope<DoctorData>;

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

/// Parsed `doctor` arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Options {
    /// `--json`.
    pub json: bool,
    /// `--live` and its settings.
    pub live: Option<Live>,
}

const USAGE: &str = "usage: doctor [--json] [--live [--database ALIAS]... \
     [--timeout-ms N] [--migrations-dir DIR]]";

/// Parse the arguments after the project flags.
///
/// Errors:
///   A usage error for unknown flags, bad values, or live options
///   without `--live`.
pub(crate) fn parse(raw: &[String]) -> Result<Options, CliError> {
    let mut options = Options::default();
    let mut live = false;
    let mut aliases = Vec::new();
    let mut timeout = None;
    let mut dir = None;
    let mut args = raw.iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let missing = || CliError::usage(format!("{flag} requires a value"));
        let mut value = || {
            let given = inline.clone().or_else(|| args.next().cloned());
            given.ok_or_else(missing)
        };
        match flag {
            "doctor" => {}
            "--json" => options.json = true,
            "--live" => live = true,
            "--database" => aliases.push(value()?),
            "--timeout-ms" => timeout = Some(millis(&value()?)?),
            "--migrations-dir" => dir = Some(PathBuf::from(value()?)),
            _ => {
                let msg = format!("unexpected argument `{arg}`; {USAGE}");
                return Err(CliError::usage(msg));
            }
        }
    }
    let live_only = !aliases.is_empty() || timeout.is_some() || dir.is_some();
    if live_only && !live {
        let msg = "--database, --timeout-ms and --migrations-dir need --live";
        return Err(CliError::usage(msg));
    }
    options.live = live.then(|| Live {
        aliases,
        timeout: timeout.unwrap_or(live::DEFAULT_TIMEOUT),
        migrations_dir: dir.unwrap_or_else(|| PathBuf::from("migrations")),
    });
    Ok(options)
}

fn millis(text: &str) -> Result<Duration, CliError> {
    match text.parse::<u64>() {
        Ok(ms) if ms > 0 => Ok(Duration::from_millis(ms)),
        _ => Err(CliError::usage("--timeout-ms needs a positive integer")),
    }
}

/// Run every offline check, then the live probes when requested.
pub(crate) async fn diagnose(probe: &Probe, live: Option<&Live>) -> Report {
    let mut findings = Findings::default();
    let found = checks::run_all(probe, &mut findings);
    if let Some(live) = live {
        match &found.settings {
            Some(settings) => {
                live::probe(settings, live, &found.dir, &mut findings).await;
            }
            None => {
                let detail = "the configuration did not load";
                let hint = "fix the `config` error first";
                findings.error("live", "doctor.E007", (detail, hint));
            }
        }
    }
    findings.into_envelope()
}

/// `siderite doctor` from the process environment.
///
/// Errors:
///   A usage error for bad arguments, or an IO error writing output.
pub(crate) async fn run(
    raw: &[String],
    cwd: PathBuf,
    project: ProjectArgs,
) -> Result<u8, CliError> {
    let options = parse(raw)?;
    let probe = Probe {
        path: std::env::var_os("PATH").unwrap_or_default(),
        cwd,
        project,
        process_env: true,
    };
    let report = diagnose(&probe, options.live.as_ref()).await;
    if options.json {
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
