//! `siderite ai validate [--json] [--live]`: the `verify` pipeline,
//! reported as a bounded list of findings for an AI tool.
//!
//! It adds no rules of its own: the steps, their order and what fails
//! are exactly `siderite verify`'s. Each finding carries its location,
//! the explanation `siderite explain` gives for its code (with the
//! documentation section or official page) and the command that
//! reproduces it. Nothing is fixed automatically, and a clean report says
//! only that these checks passed, not that a change is correct.

use crate::cargo_msg::Span;
use crate::error::CliError;
use crate::explain::{self, Entry};
use crate::report::{Envelope, Severity, channel};
use crate::verify::{self, Options, Status, Step, VerifyData};
use serde::Serialize;
use std::collections::HashSet;
use std::fmt::Write;
use std::path::Path;

const USAGE: &str = "usage: ai validate [--json] [--live]";

/// Most findings in one report.
const MAX_FINDINGS: usize = 50;

type Code = Result<u8, CliError>;

/// What a clean report does not mean.
pub(crate) const LIMITS: &str = "These checks passing does not prove a \
     change is correct: application logic still needs task-specific \
     acceptance tests.";

/// One problem to look at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Finding {
    /// The step that reported it.
    pub step: &'static str,
    /// `error` or `warning`.
    pub level: String,
    /// The diagnostic code, if any (`E0308`, `clippy::x`, `models.E003`).
    pub code: Option<String>,
    /// What is wrong.
    pub message: String,
    /// `file:line:column` of the primary span, or the framework location.
    pub location: Option<String>,
    /// What `siderite explain` says about the code.
    pub explanation: Option<Entry>,
    /// The command that shows it again.
    pub reproduce: String,
}

/// A step's outcome, without its output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct StepSummary {
    /// Step name.
    pub name: &'static str,
    /// Outcome.
    pub status: Status,
    /// The command it ran.
    pub command: String,
    /// Wall time in milliseconds.
    pub duration_ms: u64,
}

/// The `data` of an `ai validate` envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ValidateData {
    /// `offline` or `live`.
    pub profile: &'static str,
    /// Every step of `verify`.
    pub steps: Vec<StepSummary>,
    /// Problems, errors first, at most [`MAX_FINDINGS`].
    pub findings: Vec<Finding>,
    /// Findings left out by that bound.
    pub omitted_findings: usize,
    /// Commands that reproduce the failures, then the whole run.
    pub commands: Vec<String>,
    /// What passing does not mean.
    pub limits: &'static str,
}

/// Parse the arguments after the project flags.
///
/// Errors:
///   A usage error for anything but `--json` and `--live`.
pub(crate) fn parse(raw: &[String]) -> Result<Options, CliError> {
    let words = ["ai", "validate"];
    let flags = raw.iter().filter(|a| !words.contains(&a.as_str()));
    let flags: Vec<String> = flags.cloned().collect();
    verify::parse(&flags).map_err(|_| CliError::usage(USAGE))
}

/// The validation report for a `verify` envelope.
pub(crate) fn report(run: Envelope<VerifyData>) -> Envelope<ValidateData> {
    let data = &run.data;
    let mut seen = HashSet::new();
    // Lint and build report the same compiler error; keep the first.
    let mut findings: Vec<Finding> = data
        .steps
        .iter()
        .flat_map(findings_of)
        .filter(|f| seen.insert(key(f)))
        .collect();
    // Errors first; otherwise in step order.
    findings.sort_by_key(|f| f.level != "error");
    let omitted = findings.len().saturating_sub(MAX_FINDINGS);
    findings.truncate(MAX_FINDINGS);
    let mut commands: Vec<String> = data
        .steps
        .iter()
        .filter(|s| s.status == Status::Failed)
        .map(|s| s.command.clone())
        .collect();
    let mut all = String::from("siderite verify");
    if data.profile == "live" {
        all.push_str(" --live");
    }
    commands.push(all);
    let steps = data
        .steps
        .iter()
        .map(|s| StepSummary {
            name: s.name,
            status: s.status,
            command: s.command.clone(),
            duration_ms: s.duration_ms,
        })
        .collect();
    let validated = ValidateData {
        profile: data.profile,
        steps,
        findings,
        omitted_findings: omitted,
        commands,
        limits: LIMITS,
    };
    Envelope::new("ai validate", validated, run.diagnostics)
}

/// What makes two findings the same problem.
fn key(f: &Finding) -> (String, Option<String>, String, Option<String>) {
    let (level, code) = (f.level.clone(), f.code.clone());
    (level, code, f.message.clone(), f.location.clone())
}

/// The findings of one step: its compiler errors, its framework
/// diagnostics, or the step itself when it failed without either.
fn findings_of(step: &Step) -> Vec<Finding> {
    let finding = |level: &str, code: Option<&str>, message: &str| Finding {
        step: step.name,
        level: level.to_owned(),
        code: code.map(str::to_owned),
        message: message.to_owned(),
        location: None,
        explanation: code.and_then(explain::lookup),
        reproduce: step.command.clone(),
    };
    let mut out = Vec::new();
    for d in step.diagnostics.iter().filter(|d| d.level == "error") {
        let mut f = finding("error", d.code.as_deref(), &d.message);
        let primary = d.spans.iter().find(|s| s.primary);
        f.location = primary.map(Span::position);
        out.push(f);
    }
    for d in &step.framework_diagnostics {
        let level = match d.severity() {
            Severity::Error => "error",
            _ => "warning",
        };
        let mut f = finding(level, Some(d.code()), d.message());
        f.location = d.location().map(str::to_owned);
        out.push(f);
    }
    if out.is_empty() && step.status == Status::Failed {
        let message = format!("`{}` failed; see its output", step.name);
        out.push(finding("error", None, &message));
    }
    out
}

/// Text form: the steps, the findings with their pointers, the commands
/// and the limits of a clean run.
pub(crate) fn render(report: &Envelope<ValidateData>) -> String {
    let data = &report.data;
    let mut out = String::new();
    let failed = data.steps.iter().filter(|s| s.status == Status::Failed);
    let _ = writeln!(
        out,
        "ai validate ({}): {} of {} steps failed\n",
        data.profile,
        failed.count(),
        data.steps.len()
    );
    for (i, f) in data.findings.iter().enumerate() {
        let code = f.code.as_deref().map(|c| format!("[{c}]"));
        let at = f.location.as_deref().unwrap_or("-");
        let _ = writeln!(
            out,
            "{}. {}{} {at} ({}): {}",
            i + 1,
            f.level,
            code.unwrap_or_default(),
            f.step,
            f.message
        );
        if let Some(e) = &f.explanation {
            let see = e.url.as_deref().or(e.docs).unwrap_or("");
            let _ = writeln!(out, "   fix: {}", e.fix);
            if !see.is_empty() {
                let _ = writeln!(out, "   see: {see}");
            }
        }
        let _ = writeln!(out, "   reproduce: {}", f.reproduce);
    }
    if data.omitted_findings > 0 {
        let n = data.omitted_findings;
        let _ = writeln!(out, "... {n} more findings (see the JSON report)");
    }
    let _ = writeln!(out, "\nCommands:");
    for command in &data.commands {
        let _ = writeln!(out, "  {command}");
    }
    let _ = writeln!(out, "\n{LIMITS}");
    out
}

/// `siderite ai validate` for the app in `dir`.
///
/// Errors:
///   Usage and IO errors.
pub(crate) fn run(raw: &[String], dir: &Path, bin: Option<&str>) -> Code {
    let options = parse(raw)?;
    let run = verify::verify(dir, bin, options)?;
    let report = report(run);
    if options.json {
        channel::emit_json(&report, None)?;
    } else {
        print!("{}", render(&report));
    }
    Ok(report.exit_code())
}
