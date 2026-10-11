//! `siderite explain CODE [--json]` and `siderite explain --list`: what
//! a diagnostic code means, the smallest correction and the command that
//! confirms it.
//!
//! siderite's own codes (`models.E003`) come from [`catalog::CATALOG`].
//! Rust compiler codes (`E0308`) and Clippy lints (`clippy::name`) are not
//! explained here: the answer points to `rustc --explain` and the
//! official pages instead. Anything else is reported as unknown.

use crate::error::CliError;
use crate::report::{Diagnostic, Envelope, Severity, channel};
use serde::Serialize;
use std::fmt::Write;

mod catalog;
#[cfg(test)]
mod tests;

use catalog::{CATALOG, Explanation};

const USAGE: &str = "usage: explain CODE [--json] | explain --list [--json]";
const RUSTC_CODES: &str = "https://doc.rust-lang.org/error_codes";
const CLIPPY: &str = "https://rust-lang.github.io/rust-clippy";
const CLIPPY_LINTS: &str = "master/index.html";

/// Who documents a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Source {
    /// siderite.
    Siderite,
    /// The Rust compiler.
    Rustc,
    /// Clippy.
    Clippy,
}

/// One explained code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Entry {
    /// The code.
    pub code: String,
    /// Who documents it.
    pub source: Source,
    /// One line.
    pub title: String,
    /// First siderite version that emits it (siderite codes only).
    pub since: Option<&'static str>,
    /// Why it is reported.
    pub cause: String,
    /// The minimal correction.
    pub fix: String,
    /// The command that shows it is fixed.
    pub verify: String,
    /// The docs section (`siderite docs search` id), for siderite codes.
    pub docs: Option<&'static str>,
    /// The authoritative page, for compiler and Clippy codes.
    pub url: Option<String>,
}

/// The `data` of an `explain` envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ExplainData {
    /// The siderite version these explanations are for.
    pub siderite: &'static str,
    /// The explained code, or every code for `--list`.
    pub entries: Vec<Entry>,
}

/// Parsed arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    /// The code; `None` for `--list`.
    pub code: Option<String>,
    /// `--json`.
    pub json: bool,
}

/// Parse the arguments after the project flags.
///
/// Errors:
///   A usage error unless exactly one code or `--list` is given.
pub(crate) fn parse(raw: &[String]) -> Result<Options, CliError> {
    let mut json = false;
    let mut list = false;
    let mut codes = Vec::new();
    for arg in raw.iter().skip_while(|a| *a != "explain").skip(1) {
        match arg.as_str() {
            "--json" => json = true,
            "--list" => list = true,
            flag if flag.starts_with("--") => {
                let msg = format!("unexpected flag `{flag}`; {USAGE}");
                return Err(CliError::usage(msg));
            }
            code => codes.push(code.to_owned()),
        }
    }
    match (list, codes.as_slice()) {
        (true, []) => Ok(Options { code: None, json }),
        (false, [code]) => Ok(Options {
            code: Some(code.clone()),
            json,
        }),
        _ => Err(CliError::usage(USAGE)),
    }
}

/// The report for `options`.
pub(crate) fn report(options: &Options) -> Envelope<ExplainData> {
    let siderite = env!("CARGO_PKG_VERSION");
    let Some(code) = &options.code else {
        let entries = CATALOG.iter().map(framework).collect();
        let data = ExplainData { siderite, entries };
        return Envelope::new("explain", data, Vec::new());
    };
    let found = lookup(code);
    let diags = match found {
        Some(_) => Vec::new(),
        None => vec![unknown(code)],
    };
    let entries = found.into_iter().collect();
    Envelope::new("explain", ExplainData { siderite, entries }, diags)
}

/// The explanation of `code`, if it is one siderite knows how to answer.
pub(crate) fn lookup(code: &str) -> Option<Entry> {
    let code = code.trim();
    let same = |e: &&Explanation| e.code.eq_ignore_ascii_case(code);
    if let Some(found) = CATALOG.iter().find(same) {
        return Some(framework(found));
    }
    if let Some(lint) = code.strip_prefix("clippy::") {
        return is_lint(lint).then(|| clippy(lint));
    }
    is_rustc(code).then(|| rustc(&code.to_ascii_uppercase()))
}

fn framework(e: &Explanation) -> Entry {
    Entry {
        code: e.code.to_owned(),
        source: Source::Siderite,
        title: e.title.to_owned(),
        since: Some(e.since),
        cause: e.cause.to_owned(),
        fix: e.fix.to_owned(),
        verify: e.verify.to_owned(),
        docs: Some(e.docs),
        url: None,
    }
}

/// `E` and four digits.
fn is_rustc(code: &str) -> bool {
    let mut chars = code.chars();
    let letter = chars.next().is_some_and(|c| c.eq_ignore_ascii_case(&'e'));
    let digits: Vec<char> = chars.collect();
    letter && digits.len() == 4 && digits.iter().all(char::is_ascii_digit)
}

fn is_lint(name: &str) -> bool {
    let lint_char = |c: char| c.is_ascii_lowercase() || c == '_';
    !name.is_empty() && name.chars().all(lint_char)
}

fn rustc(code: &str) -> Entry {
    Entry {
        code: code.to_owned(),
        source: Source::Rustc,
        title: format!("Rust compiler error {code}"),
        since: None,
        cause: "A Rust compiler error; the compiler documents it, not \
                siderite."
            .to_owned(),
        fix: format!("Run `rustc --explain {code}` or read the page below."),
        verify: "siderite build".to_owned(),
        docs: None,
        url: Some(format!("{RUSTC_CODES}/{code}.html")),
    }
}

fn clippy(lint: &str) -> Entry {
    Entry {
        code: format!("clippy::{lint}"),
        source: Source::Clippy,
        title: format!("Clippy lint {lint}"),
        since: None,
        cause: "A Clippy lint; Clippy documents it, not siderite.".to_owned(),
        fix: "Follow the lint's page below; `cargo clippy --fix` applies \
              the machine-applicable suggestions."
            .to_owned(),
        verify: "siderite lint -- -D warnings".to_owned(),
        docs: None,
        url: Some(format!("{CLIPPY}/{CLIPPY_LINTS}#{lint}")),
    }
}

fn unknown(code: &str) -> Diagnostic {
    let message = format!(
        "`{code}` is not a siderite, rustc (E0000) or Clippy \
         (clippy::name) code"
    );
    let hint = "copy the code exactly; `siderite explain --list` shows \
                siderite's codes";
    Diagnostic::new("explain.E001", Severity::Error, &message).with_hint(hint)
}

/// Text form of one entry, or a table for `--list`.
pub(crate) fn render(report: &Envelope<ExplainData>, list: bool) -> String {
    let mut out = String::new();
    if list {
        for e in &report.data.entries {
            let _ = writeln!(out, "{:<16} {}", e.code, e.title);
        }
        return out;
    }
    for e in &report.data.entries {
        let _ = writeln!(out, "{}: {}\n", e.code, e.title);
        let _ = writeln!(out, "Cause: {}\n", e.cause);
        let _ = writeln!(out, "Fix: {}\n", e.fix);
        let _ = writeln!(out, "Verify: {}", e.verify);
        if let Some(docs) = e.docs {
            let _ = writeln!(out, "Docs: {docs}");
        }
        if let Some(url) = &e.url {
            let _ = writeln!(out, "See: {url}");
        }
        if let Some(since) = e.since {
            let version = report.data.siderite;
            let current = format!("this is {version}");
            let _ = writeln!(out, "Since: siderite {since} ({current})");
        }
    }
    out
}

/// `siderite explain`.
///
/// Errors:
///   Usage errors, or an IO error writing the report.
pub(crate) fn run(raw: &[String]) -> Result<u8, CliError> {
    let options = parse(raw)?;
    let report = report(&options);
    if options.json {
        channel::emit_json(&report, None)?;
        return Ok(report.exit_code());
    }
    for diag in &report.diagnostics {
        eprintln!("error: {}", diag.message());
    }
    print!("{}", render(&report, options.code.is_none()));
    Ok(report.exit_code())
}
