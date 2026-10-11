//! `siderite openapi diff BASE CURRENT [--json]`: what changed between
//! two exported OpenAPI documents (local JSON files) for existing
//! clients.
//!
//! Operations are matched by method and path template (parameter names
//! ignored, so `/a/{id}` and `/a/{key}` are the same path). Reported:
//! removed and added operations; parameters that are new and required,
//! became required or were removed; request bodies that became required
//! or lost a media type; responses and media types that were removed or
//! added; and schema changes in either direction (see [`schema`]). Only
//! references inside the document (`#/...`) are followed: anything else
//! is unknown, never fetched. Security requirements and callbacks are
//! not compared: when they differ the change is unknown. Unknown counts
//! as incompatible, so the command exits `1` for breaking or unknown
//! changes and `0` when everything is additive or equal.

use crate::error::CliError;
use crate::fsio::io_error;
use crate::report::text::diagnostic_line;
use crate::report::{Diagnostic, Envelope, Severity, channel};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};

pub(crate) mod model;
mod operation;
pub(crate) mod schema;
#[cfg(test)]
mod tests;

use model::{Change, Impact, Pair};

/// A change's stable identifier.
type Kind = &'static str;

const USAGE: &str = "usage: openapi diff BASE CURRENT [--json]";

/// Parsed arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    /// The earlier document.
    pub base: PathBuf,
    /// The later document.
    pub current: PathBuf,
    /// `--json`.
    pub json: bool,
}

/// Parse the arguments after the project flags (`openapi diff ...`).
///
/// Errors:
///   A usage error unless exactly two paths are given.
pub(crate) fn parse(raw: &[String], cwd: &Path) -> Result<Options, CliError> {
    let rest = raw.iter().skip_while(|a| *a != "diff").skip(1);
    let mut json = false;
    let mut paths = Vec::new();
    for arg in rest {
        match arg.as_str() {
            "--json" => json = true,
            flag if flag.starts_with('-') => {
                let msg = format!("unexpected flag `{flag}`; {USAGE}");
                return Err(CliError::usage(msg));
            }
            path => paths.push(cwd.join(path)),
        }
    }
    let usage = |_: Vec<PathBuf>| CliError::usage(USAGE);
    let [base, current]: [PathBuf; 2] = paths.try_into().map_err(usage)?;
    Ok(Options {
        base,
        current,
        json,
    })
}

/// The comparison in progress.
pub(crate) struct Diff<'a> {
    base: &'a Value,
    current: &'a Value,
    operation: Option<String>,
    seen: HashSet<(String, String)>,
    changes: Vec<Change>,
}

impl<'a> Diff<'a> {
    fn new(base: &'a Value, current: &'a Value) -> Self {
        Self {
            base,
            current,
            operation: None,
            seen: HashSet::new(),
            changes: Vec::new(),
        }
    }

    fn push(&mut self, impact: Impact, kind: Kind, message: String) {
        self.changes.push(Change {
            impact,
            kind,
            operation: self.operation.clone(),
            message,
            base: None,
            current: None,
        });
    }

    fn at(&mut self, base: Option<&str>, current: Option<&str>) {
        if let Some(last) = self.changes.last_mut() {
            last.base = base.map(str::to_owned);
            last.current = current.map(str::to_owned);
        }
    }

    /// A change that breaks existing clients, found at `p`.
    pub(crate) fn breaking(&mut self, kind: Kind, msg: String, p: Pair<'_>) {
        self.push(Impact::Breaking, kind, msg);
        self.at(Some(p.b.at), Some(p.c.at));
    }

    /// A change that could not be compared, found at `p`.
    pub(crate) fn unknown(&mut self, kind: Kind, msg: String, p: Pair<'_>) {
        self.push(Impact::Unknown, kind, msg);
        self.at(Some(p.b.at), Some(p.c.at));
    }

    /// Something new in CURRENT at `at`.
    pub(crate) fn added(&mut self, kind: Kind, msg: String, at: &str) {
        self.push(Impact::Additive, kind, msg);
        self.at(None, Some(at));
    }

    /// Something only BASE has, at `at`.
    fn removed(&mut self, impact: Impact, kind: Kind, msg: String, at: &str) {
        self.push(impact, kind, msg);
        self.at(Some(at), None);
    }

    /// Something only CURRENT has that breaks clients, at `at`.
    fn required(&mut self, kind: Kind, msg: String, at: &str) {
        self.push(Impact::Breaking, kind, msg);
        self.at(None, Some(at));
    }
}

/// Counts by impact.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Summary {
    /// Breaking changes.
    pub breaking: usize,
    /// Changes that were not compared or are undecidable.
    pub unknown: usize,
    /// Additive changes.
    pub additive: usize,
}

/// The `data` of an `openapi diff` envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DiffData {
    /// The BASE file.
    pub base: String,
    /// The CURRENT file.
    pub current: String,
    /// Counts by impact.
    pub summary: Summary,
    /// Every change: breaking first, then unknown, then additive.
    pub changes: Vec<Change>,
}

/// Every change from `base` to `current`, most severe first, in document
/// order within an impact.
pub(crate) fn compare(base: &Value, current: &Value) -> Vec<Change> {
    let mut diff = Diff::new(base, current);
    diff.document();
    let mut changes = diff.changes;
    changes.sort_by_key(|c| c.impact);
    changes
}

type Reported = Result<Envelope<DiffData>, CliError>;

/// The report for two documents read from `options`' files.
///
/// Errors:
///   An IO error, or a usage error for a file that is not an OpenAPI 3
///   JSON document.
pub(crate) fn report(options: &Options) -> Reported {
    let base = load(&options.base)?;
    let current = load(&options.current)?;
    let changes = compare(&base, &current);
    let mut summary = Summary::default();
    for change in &changes {
        match change.impact {
            Impact::Breaking => summary.breaking += 1,
            Impact::Unknown => summary.unknown += 1,
            Impact::Additive => summary.additive += 1,
        }
    }
    let mut diags = Vec::new();
    if summary.breaking > 0 {
        let n = summary.breaking;
        let msg = format!("{n} breaking change(s) for existing clients");
        diags.push(Diagnostic::new("openapi.E002", Severity::Error, &msg));
    }
    if summary.unknown > 0 {
        let n = summary.unknown;
        let msg = format!("{n} change(s) could not be compared");
        let diag = Diagnostic::new("openapi.E003", Severity::Error, &msg);
        let hint = "review them by hand; unknown is not compatible";
        diags.push(diag.with_hint(hint));
    }
    let data = DiffData {
        base: options.base.display().to_string(),
        current: options.current.display().to_string(),
        summary,
        changes,
    };
    Ok(Envelope::new("openapi diff", data, diags))
}

fn load(path: &Path) -> Result<Value, CliError> {
    let text = std::fs::read_to_string(path);
    let text = text.map_err(|err| io_error("read", path, &err))?;
    let shown = path.display();
    let invalid = |err| CliError::usage(format!("{shown}: {err}"));
    let value: Value = serde_json::from_str(&text).map_err(invalid)?;
    let version = value.get("openapi").and_then(Value::as_str);
    if !version.is_some_and(|v| v.starts_with("3.")) {
        let msg = format!("{shown} is not an OpenAPI 3 document");
        return Err(CliError::usage(msg));
    }
    Ok(value)
}

/// Text form: one line per change, grouped by impact, then the counts.
pub(crate) fn render(report: &Envelope<DiffData>) -> String {
    let data = &report.data;
    let mut out = String::new();
    let mut grouped: BTreeMap<Impact, Vec<&Change>> = BTreeMap::new();
    for change in &data.changes {
        grouped.entry(change.impact).or_default().push(change);
    }
    for (impact, changes) in grouped {
        let title = format!("{impact:?}").to_lowercase();
        let _ = writeln!(out, "{title}:");
        for change in changes {
            let place = change.operation.as_deref().unwrap_or("document");
            let _ = writeln!(out, "  {place}: {}", change.message);
            let at = change.current.as_ref().or(change.base.as_ref());
            if let Some(at) = at {
                let _ = writeln!(out, "    at {at}");
            }
        }
    }
    let Summary {
        breaking,
        unknown,
        additive,
    } = data.summary;
    let _ = writeln!(
        out,
        "{breaking} breaking, {unknown} unknown, {additive} additive"
    );
    for diag in &report.diagnostics {
        let _ = writeln!(out, "{}", diagnostic_line(diag));
    }
    out
}

/// `siderite openapi diff` from `cwd`.
///
/// Errors:
///   Usage errors, unreadable or invalid documents, or an IO error
///   writing the report.
pub(crate) fn run(raw: &[String], cwd: &Path) -> Result<u8, CliError> {
    let options = parse(raw, cwd)?;
    let report = report(&options)?;
    if options.json {
        channel::emit_json(&report, None)?;
    } else {
        print!("{}", render(&report));
    }
    Ok(report.exit_code())
}
