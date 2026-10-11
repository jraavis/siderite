//! `siderite verify [--json] [--live]`: the whole local verification in
//! one command, with one report.
//!
//! Steps, in order, through the same runners as the single commands:
//! `fmt` (`cargo fmt --check`, never rewriting files), `lint` (Clippy with
//! warnings denied), `build` (all targets), `test` and `check` (the
//! framework checks of the app). A failed build skips the steps that need
//! it. Offline is the default profile; `--live` also runs the ignored
//! (live-service) tests. No migration is ever applied.
//!
//! Lint, build and test run with cargo's JSON message format, so their
//! compiler diagnostics keep codes, spans and rendered explanations
//! (versioned by `diagnostic_format`). Per-test results need unstable
//! libtest options, so test outcomes are reported per step only
//! (`test_results: "aggregate"`), not parsed from terminal output.

use crate::cargo_msg::{CompilerDiagnostic, DIAGNOSTIC_FORMAT};
use crate::error::CliError;
use crate::project::{self, ReportFile, passthrough};
use crate::report::channel::REPORT_PATH_ENV;
use crate::report::{Diagnostic, Envelope, Severity, channel, redact_text};
use crate::workspace::spawn_error;
use serde::Serialize;
use std::fmt::Write;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

mod capture;

use capture::{Captured, capture};

type Code = Result<u8, CliError>;

const JSON_FORMAT: &str = "--message-format=json";

/// How many trailing output lines a failed step keeps.
const TAIL: usize = 30;

/// Outcome of one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    /// It succeeded.
    Passed,
    /// It failed.
    Failed,
    /// It did not run because an earlier step it needs failed.
    Skipped,
}

/// One step of the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Step {
    /// `fmt`, `lint`, `build`, `test` or `check`.
    pub name: &'static str,
    /// The command that ran, for reproducing it.
    pub command: String,
    /// Outcome.
    pub status: Status,
    /// Wall time in milliseconds.
    pub duration_ms: u64,
    /// The child's exit code, when it ran.
    pub exit_code: Option<i32>,
    /// The last lines of a failed step's output that are not compiler
    /// diagnostics (redacted).
    pub output: Option<String>,
    /// Distinct compiler diagnostics (rustc and Clippy), errors first,
    /// at most [`capture::MAX_DIAGNOSTICS`], redacted.
    pub diagnostics: Vec<CompilerDiagnostic>,
    /// Diagnostics left out by that bound.
    pub omitted_diagnostics: usize,
}

/// The `data` of a `verify` envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct VerifyData {
    /// `offline` or `live`.
    pub profile: &'static str,
    /// Version of the compiler diagnostic shape.
    pub diagnostic_format: u32,
    /// `aggregate`: stable Rust has no structured per-test output, so a
    /// test step reports the outcome of `cargo test` as a whole.
    pub test_results: &'static str,
    /// Every step, in order.
    pub steps: Vec<Step>,
}

/// Parsed `verify` arguments.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Options {
    /// `--json`.
    pub json: bool,
    /// `--live`.
    pub live: bool,
}

/// Parse the arguments after the project flags.
///
/// Errors:
///   A usage error for anything else.
pub(crate) fn parse(raw: &[String]) -> Result<Options, CliError> {
    let mut options = Options::default();
    for arg in raw {
        match arg.as_str() {
            "verify" => {}
            "--json" => options.json = true,
            "--live" => options.live = true,
            other => {
                let msg = format!(
                    "unexpected argument `{other}`; usage: verify [--json] \
                     [--live]"
                );
                return Err(CliError::usage(msg));
            }
        }
    }
    Ok(options)
}

/// A step to run: its name, Cargo arguments, whether it needs a
/// successful build and whether cargo reports in JSON.
struct Plan {
    name: &'static str,
    args: Vec<String>,
    needs_build: bool,
    json: bool,
}

/// `plan.args` with cargo's JSON message format, which goes before any
/// `--` (after it, the arguments belong to Clippy or the tests).
fn cargo_args(plan: &Plan) -> Vec<String> {
    let mut args = plan.args.clone();
    if plan.json {
        let at = args.iter().position(|a| a == "--").unwrap_or(args.len());
        args.insert(at, JSON_FORMAT.to_owned());
    }
    args
}

/// `list` as owned strings.
fn owned(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

fn plans(live: bool, bin: Option<&str>) -> Vec<Plan> {
    let mut test = owned(&["test", "--all-targets"]);
    if live {
        test.extend(owned(&["--", "--include-ignored"]));
    }
    let mut check = owned(&["run"]);
    if let Some(bin) = bin {
        check.extend(owned(&["--bin", bin]));
    }
    check.extend(owned(&["--", "check", "--json"]));
    vec![
        Plan {
            name: "fmt",
            args: owned(&["fmt", "--check"]),
            needs_build: false,
            json: false,
        },
        Plan {
            name: "lint",
            args: owned(&["clippy", "--all-targets", "--", "-D", "warnings"]),
            needs_build: false,
            json: true,
        },
        Plan {
            name: "build",
            args: owned(&["build", "--all-targets"]),
            needs_build: false,
            json: true,
        },
        Plan {
            name: "test",
            args: test,
            needs_build: true,
            json: true,
        },
        Plan {
            name: "check",
            args: check,
            needs_build: true,
            json: false,
        },
    ]
}

/// Run every step in `dir` and return the `verify` envelope.
///
/// Errors:
///   Only an IO error creating the report file of the `check` step.
pub(crate) fn verify(
    dir: &Path,
    bin: Option<&str>,
    options: Options,
) -> Result<Envelope<VerifyData>, CliError> {
    let mut steps = Vec::new();
    let mut diags = Vec::new();
    let mut built = true;
    for plan in plans(options.live, bin) {
        let shown = format!("cargo {}", plan.args.join(" "));
        if plan.needs_build && !built {
            steps.push(skipped(plan.name, shown));
            continue;
        }
        eprintln!("verify: {} ({shown})", plan.name);
        let step = run_step(dir, &plan, shown)?;
        if step.status == Status::Failed {
            if plan.name == "build" {
                built = false;
            }
            diags.push(failure(&step));
        }
        steps.push(step);
    }
    let profile = if options.live { "live" } else { "offline" };
    let data = VerifyData {
        profile,
        diagnostic_format: DIAGNOSTIC_FORMAT,
        test_results: "aggregate",
        steps,
    };
    Ok(Envelope::new("verify", data, diags))
}

fn skipped(name: &'static str, command: String) -> Step {
    Step {
        name,
        command,
        status: Status::Skipped,
        duration_ms: 0,
        exit_code: None,
        output: Some("skipped: the build failed".to_owned()),
        diagnostics: Vec::new(),
        omitted_diagnostics: 0,
    }
}

fn failure(step: &Step) -> Diagnostic {
    let message = format!("`{}` failed: {}", step.name, step.command);
    let hint = format!("run `{}` to see the full output", step.command);
    Diagnostic::new("verify.E001", Severity::Error, &message)
        .with_location(step.name)
        .with_hint(&hint)
}

fn run_step(dir: &Path, plan: &Plan, shown: String) -> Result<Step, CliError> {
    let pending = Pending {
        name: plan.name,
        command: shown,
        started: Instant::now(),
    };
    if let Some(err) = missing_component(plan.name) {
        return Ok(finish(pending, None, text(err.to_string())));
    }
    let mut command = Command::new("cargo");
    command.args(cargo_args(plan)).current_dir(dir);
    let report = (plan.name == "check").then(ReportFile::new).transpose()?;
    if let Some(report) = &report {
        command.env(REPORT_PATH_ENV, report.path());
    }
    let (code, mut captured) = match command.output() {
        Ok(Output {
            status,
            stdout,
            stderr,
        }) => {
            let stdout = String::from_utf8_lossy(&stdout);
            let stderr = String::from_utf8_lossy(&stderr);
            (status.code(), capture(&stdout, &stderr, plan.json))
        }
        Err(err) => {
            let what = &pending.command;
            (None, text(spawn_error(what, &err).to_string()))
        }
    };
    // A failed framework check is explained by its report.
    if code != Some(0)
        && let Some(found) = report.as_ref().and_then(|r| r.read(true))
    {
        captured.text = found;
    }
    Ok(finish(pending, code, captured))
}

/// The error for a `fmt` or `lint` step whose rustup component is
/// missing, so the step fails with the fix instead of a Cargo error.
fn missing_component(name: &str) -> Option<CliError> {
    let pass = passthrough(name)?;
    let component = pass.component?;
    project::require_subcommand(&pass, component).err()
}

/// A step that has started.
struct Pending {
    name: &'static str,
    command: String,
    started: Instant,
}

/// Plain text output, without compiler diagnostics.
fn text(text: String) -> Captured {
    Captured {
        text,
        ..Captured::default()
    }
}

fn finish(pending: Pending, code: Option<i32>, out: Captured) -> Step {
    let status = if code == Some(0) {
        Status::Passed
    } else {
        Status::Failed
    };
    let output = (status == Status::Failed).then(|| tail(&out.text));
    let millis = pending.started.elapsed().as_millis();
    Step {
        name: pending.name,
        command: pending.command,
        status,
        duration_ms: u64::try_from(millis).unwrap_or(u64::MAX),
        exit_code: code,
        output,
        diagnostics: out.diagnostics,
        omitted_diagnostics: out.omitted,
    }
}

/// The last [`TAIL`] lines of `text`, redacted.
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(TAIL);
    redact_text(&lines[start..].join("\n"))
}

/// Text form: one line per step, failure output, then a summary.
pub(crate) fn render(report: &Envelope<VerifyData>) -> String {
    let mut out = String::new();
    for step in &report.data.steps {
        let status = match step.status {
            Status::Passed => "ok",
            Status::Failed => "FAILED",
            Status::Skipped => "skipped",
        };
        let secs = Duration::from_millis(step.duration_ms).as_secs_f64();
        let _ = writeln!(
            out,
            "{status:<8} {:<6} {secs:>7.1}s  {}",
            step.name, step.command
        );
        if step.status == Status::Failed {
            failure_text(&mut out, step);
        }
    }
    let failed = report.diagnostics.len();
    let total = report.data.steps.len();
    let profile = report.data.profile;
    let counts = format!("{failed} of {total} steps failed");
    let _ = writeln!(out, "verify ({profile}): {counts}");
    out
}

/// A failed step's error diagnostics as rustc renders them, then the
/// rest of its output, indented.
fn failure_text(out: &mut String, step: &Step) {
    let errors = step.diagnostics.iter().filter(|d| d.level == "error");
    let rendered = errors.filter_map(|d| d.rendered.as_deref());
    let output = step.output.as_deref().into_iter();
    for line in rendered.chain(output).flat_map(str::lines) {
        let _ = writeln!(out, "    {line}");
    }
}

/// `siderite verify` for the app in `dir`.
///
/// Errors:
///   Usage and IO errors.
pub(crate) fn run(raw: &[String], dir: &Path, bin: Option<&str>) -> Code {
    let options = parse(raw)?;
    let report = verify(dir, bin, options)?;
    if options.json {
        channel::emit_json(&report, None)?;
    } else {
        print!("{}", render(&report));
    }
    Ok(report.exit_code())
}

#[cfg(test)]
mod tests;
