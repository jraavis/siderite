//! `siderite docs search QUERY [--json] [--limit N]`: the framework's
//! documentation for this version, searched offline.
//!
//! The index is packaged with the CLI (see [`index`]), so results always
//! describe the version of siderite that printed them. Each result names
//! its source file and line, a link to that file at the release tag, the
//! Cargo features the section says it needs and whether it is an example
//! compiled in the framework's own build. When the project in the current
//! directory locks a different siderite version, the report says so.

use crate::error::CliError;
use crate::report::{Diagnostic, Envelope, Severity, channel};
use serde::Serialize;
use std::fmt::Write;
use std::path::Path;

pub(crate) mod index;
pub(crate) mod version;

#[cfg(test)]
mod generate;
#[cfg(test)]
mod tests;

use index::{Index, Kind, Scored, clip, excerpt, search, terms};

/// Results shown when `--limit` is not given.
const DEFAULT_LIMIT: usize = 5;
/// The most results one search returns.
const MAX_LIMIT: usize = 50;
/// Characters of section text a JSON result carries.
const MAX_TEXT: usize = 4000;
/// Matching lines in a result's excerpt.
const EXCERPT_LINES: usize = 3;

type Report = Envelope<DocsData>;

const USAGE: &str = "usage: docs search QUERY... [--json] [--limit N]";

/// Parsed `docs search` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    /// The query words.
    pub query: String,
    /// `--json`.
    pub json: bool,
    /// `--limit`.
    pub limit: usize,
}

/// Parse the arguments after the project flags (`docs search ...`).
///
/// Errors:
///   A usage error for a missing subcommand or query, or a bad flag.
pub(crate) fn parse(raw: &[String]) -> Result<Options, CliError> {
    let mut args = raw.iter().skip_while(|a| *a != "docs").skip(1);
    if args.next().map(String::as_str) != Some("search") {
        return Err(CliError::usage(USAGE));
    }
    let mut words = Vec::new();
    let mut options = Options {
        query: String::new(),
        json: false,
        limit: DEFAULT_LIMIT,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => options.json = true,
            "--limit" => {
                let value = args.next().map(String::as_str);
                options.limit = limit(value.unwrap_or(""))?;
            }
            flag if flag.starts_with("--limit=") => {
                options.limit = limit(&flag["--limit=".len()..])?;
            }
            flag if flag.starts_with('-') => {
                let msg = format!("unexpected flag `{flag}`; {USAGE}");
                return Err(CliError::usage(msg));
            }
            word => words.push(word),
        }
    }
    options.query = words.join(" ");
    if index::terms(&options.query).is_empty() {
        return Err(CliError::usage(USAGE));
    }
    Ok(options)
}

fn limit(value: &str) -> Result<usize, CliError> {
    match value.parse::<usize>() {
        Ok(n) if (1..=MAX_LIMIT).contains(&n) => Ok(n),
        _ => Err(CliError::usage(format!(
            "--limit takes a number from 1 to {MAX_LIMIT}"
        ))),
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Hit {
    /// Stable id (page path and anchor, or the example's path).
    pub id: String,
    /// `guide` or `example`.
    pub kind: Kind,
    /// Page title and section heading.
    pub title: String,
    /// Source file, relative to the repository root.
    pub path: String,
    /// First line of the section.
    pub line: u32,
    /// The source at this version's release tag.
    pub source_url: String,
    /// Cargo features the section says it needs.
    pub features: Vec<String>,
    /// Whether the code is compiled in the framework's own build.
    pub compiled: bool,
    /// Relevance; higher is better.
    pub score: f64,
    /// Lines that contain query words.
    pub excerpt: Vec<String>,
    /// The section, cut to a bounded length.
    pub text: String,
    /// Whether `text` was cut.
    pub truncated: bool,
}

/// The `data` of a `docs` envelope.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct DocsData {
    /// The framework version these docs describe.
    pub docs_version: String,
    /// Version of the index layout.
    pub index_format: u32,
    /// siderite versions locked by the project here, if any.
    pub installed: Vec<String>,
    /// The query as given.
    pub query: String,
    /// How many sections matched.
    pub total: usize,
    /// The best matches, best first.
    pub results: Vec<Hit>,
}

/// Search `index` for `options` from a project in `dir`.
pub(crate) fn report(index: &Index, options: &Options, dir: &Path) -> Report {
    let terms = terms(&options.query);
    let found = search(index, &terms);
    let results = found
        .iter()
        .take(options.limit)
        .map(|s| hit(s, &terms, &index.siderite))
        .collect();
    let installed = version::installed(dir);
    let diags = mismatch(&index.siderite, &installed).into_iter().collect();
    let data = DocsData {
        docs_version: index.siderite.clone(),
        index_format: index.format,
        installed,
        query: options.query.clone(),
        total: found.len(),
        results,
    };
    Envelope::new("docs search", data, diags)
}

fn hit(scored: &Scored, terms: &[String], version: &str) -> Hit {
    let entry = scored.entry;
    let text = clip(&entry.text, MAX_TEXT);
    let repo = env!("CARGO_PKG_REPOSITORY");
    let (path, line) = (&entry.path, entry.line);
    Hit {
        id: entry.id.clone(),
        kind: entry.kind,
        title: entry.title.clone(),
        path: path.clone(),
        line,
        source_url: format!("{repo}/blob/v{version}/{path}#L{line}"),
        features: entry.features.clone(),
        compiled: entry.kind == Kind::Example,
        score: (scored.score * 1000.0).round() / 1000.0,
        excerpt: excerpt(&entry.text, terms, EXCERPT_LINES),
        truncated: text.len() != entry.text.len(),
        text,
    }
}

/// A warning when the project builds with another siderite version.
fn mismatch(docs: &str, installed: &[String]) -> Option<Diagnostic> {
    if installed.is_empty() || installed.iter().any(|v| v == docs) {
        return None;
    }
    let locked = installed.join(", ");
    let message = format!(
        "these docs are for siderite {docs}, but the project's Cargo.lock \
         has siderite {locked}"
    );
    let first = &installed[0];
    let hint = format!(
        "use the CLI matching the project: cargo install siderite-cli \
         --version {first}"
    );
    let diag = Diagnostic::new("docs.W001", Severity::Warning, &message);
    Some(diag.with_location("Cargo.lock").with_hint(&hint))
}

/// Text form: the version line, then each result with its location and
/// matching lines.
pub(crate) fn render(report: &Envelope<DocsData>) -> String {
    let data = &report.data;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "siderite {} docs: {} of {} matches for \"{}\"",
        data.docs_version,
        data.results.len(),
        data.total,
        data.query
    );
    for hit in &data.results {
        let _ = writeln!(out, "\n{}  ({})", hit.title, hit.id);
        let _ = writeln!(out, "  {}:{}", hit.path, hit.line);
        if !hit.features.is_empty() {
            let _ = writeln!(out, "  features: {}", hit.features.join(", "));
        }
        for line in &hit.excerpt {
            let _ = writeln!(out, "  | {line}");
        }
    }
    out
}

/// The ids of the packaged index, for checks of links into the docs.
#[cfg(test)]
pub(crate) fn packaged_ids() -> std::collections::HashSet<String> {
    let index = index::packaged().unwrap_or_else(|e| panic!("{e}"));
    index.entries.into_iter().map(|e| e.id).collect()
}

/// `siderite docs search` from `cwd`.
///
/// Errors:
///   Usage errors, or an IO error writing the report.
pub(crate) fn run(raw: &[String], cwd: &Path) -> Result<u8, CliError> {
    let options = parse(raw)?;
    let index = index::packaged().map_err(CliError::Io)?;
    let report = report(&index, &options, cwd);
    if options.json {
        channel::emit_json(&report, None)?;
        return Ok(report.exit_code());
    }
    for diag in &report.diagnostics {
        eprintln!("warning: {}", diag.message());
    }
    print!("{}", render(&report));
    Ok(report.exit_code())
}
