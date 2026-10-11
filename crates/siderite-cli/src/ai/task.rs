//! `ai context --task TEXT [--budget N] [--include PATH]...`: context for
//! one task, within a stated budget.
//!
//! Parts are chosen by local keyword matching against the task: a
//! project summary, the models and routes whose names match, the files
//! the user named with `--include`, then the best documentation sections
//! and examples of this siderite version. The budget is in bytes of the
//! rendered Markdown; parts are taken in that order, the first one that
//! does not fit is cut at a line boundary and later ones that do not fit
//! are listed as omitted, so the same inputs always give the same output.
//! Nothing leaves the machine.

use super::files::{self, Collected};
use super::markdown::{cell, model, route_table};
use super::{Format, format};
use crate::docs::index::{self, Index, Kind};
use crate::error::CliError;
use crate::report::snapshot::Snapshot;
use crate::report::{ModelReport, RouteReport};
use serde::Serialize;
use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::{Path, PathBuf};

/// Budget when `--budget` is not given.
pub(crate) const DEFAULT_BUDGET: usize = 32 << 10;
const MIN_BUDGET: usize = 4 << 10;
const MAX_BUDGET: usize = 4 << 20;
/// Bytes kept for the list of omitted and missing items.
const TAIL_RESERVE: usize = 1536;
/// A part is cut, not dropped, when at least this much room is left.
const MIN_CUT: usize = 400;
const DOCS: usize = 4;
const FENCE: &str = "```\n";
const EXAMPLES: usize = 2;
/// Words too common to select anything.
const STOP: &str = "the and for with add new from into that this make use \
                    should when each all can how";
/// Always missing: this command never reads the database.
const NO_STATUS: &str = "migration status (offline; run \
                         `siderite showmigrations`)";

type Words = [String];
type Notes = Vec<String>;
type Fitted = (Vec<Part>, Vec<Omitted>);

const USAGE: &str = "usage: ai context --task TEXT [--budget BYTES] \
     [--include PATH]... [--format markdown|json]";

/// Parsed `ai context` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    /// The task, in the user's words.
    pub task: String,
    /// Budget in bytes.
    pub budget: usize,
    /// Paths to include, as given.
    pub include: Vec<PathBuf>,
    /// Output format.
    pub format: Format,
}

/// Parse `rest` (`["ai", "context", ...]`); `json` is the global flag.
///
/// Errors:
///   A usage error for a missing task or a bad flag.
pub(crate) fn parse(rest: &[String], json: bool) -> Result<Options, CliError> {
    let mut options = Options {
        task: String::new(),
        budget: DEFAULT_BUDGET,
        include: Vec::new(),
        format: if json { Format::Json } else { Format::Markdown },
    };
    let mut args = rest.iter().skip(2);
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| args.next().cloned());
        match flag {
            "--task" => options.task = value().unwrap_or_default(),
            "--budget" => options.budget = budget(value())?,
            "--include" => {
                let path = value().ok_or_else(|| CliError::usage(USAGE))?;
                options.include.push(PathBuf::from(path));
            }
            "--format" => options.format = format(value().as_deref())?,
            _ => {
                let msg = format!("unexpected argument `{arg}`; {USAGE}");
                return Err(CliError::usage(msg));
            }
        }
    }
    if options.task.trim().is_empty() {
        return Err(CliError::usage(USAGE));
    }
    Ok(options)
}

fn budget(value: Option<String>) -> Result<usize, CliError> {
    match value.as_deref().map(str::parse::<usize>) {
        Some(Ok(n)) if (MIN_BUDGET..=MAX_BUDGET).contains(&n) => Ok(n),
        _ => Err(CliError::usage(format!(
            "--budget is a number of bytes from {MIN_BUDGET} to {MAX_BUDGET}"
        ))),
    }
}

/// One part of the context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Part {
    /// `project`, `models`, `routes`, `file`, `docs` or `example`.
    pub kind: &'static str,
    /// Heading.
    pub title: String,
    /// Where it comes from, with its version.
    pub source: String,
    /// Markdown body.
    pub text: String,
    /// Whether the body was cut to fit the budget.
    pub truncated: bool,
}

/// A part left out by the budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Omitted {
    /// Kind, as in [`Part`].
    pub kind: &'static str,
    /// Heading.
    pub title: String,
    /// Where it comes from.
    pub source: String,
    /// Its rendered size in bytes.
    pub bytes: usize,
}

/// The budget and its use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct Budget {
    /// Always `bytes` (of the Markdown form).
    pub unit: &'static str,
    /// The limit.
    pub limit: usize,
    /// Bytes used.
    pub used: usize,
}

/// The `data` of an `ai context` envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TaskContext {
    /// The task.
    pub task: String,
    /// siderite version of the app.
    pub siderite: String,
    /// Version the documentation parts describe.
    pub docs_version: String,
    /// The budget.
    pub budget: Budget,
    /// Included parts, in priority order.
    pub parts: Vec<Part>,
    /// Parts that did not fit.
    pub omitted: Vec<Omitted>,
    /// What the context does not contain and where to get it.
    pub missing: Vec<String>,
    /// The Markdown form, within the budget.
    pub markdown: String,
}

/// What the context is built from.
pub(crate) struct Sources<'a> {
    /// The app snapshot.
    pub snapshot: &'a Snapshot,
    /// Notes on the snapshot (its diagnostics' messages).
    pub notes: Vec<String>,
    /// The documentation index.
    pub index: &'a Index,
    /// The project root for `--include`.
    pub root: &'a Path,
}

/// Build the context for `options`.
pub(crate) fn build(sources: &Sources, options: &Options) -> TaskContext {
    let task_terms = task_words(&options.task);
    let snap = sources.snapshot;
    let siderite = snap.framework.version.clone();
    let mut missing = vec![NO_STATUS.to_owned()];
    missing.extend(sources.notes.iter().cloned());
    let mut parts = vec![project(snap)];
    parts.extend(models(snap, &task_terms, &mut missing));
    parts.extend(routes(snap, &task_terms));
    let collected = files::collect(sources.root, &options.include);
    parts.extend(file_parts(&collected, &mut missing));
    if options.include.is_empty() {
        missing.push("source files (pass --include PATH)".to_owned());
    }
    parts.extend(docs(sources.index, &task_terms, &mut missing));
    let header = header(&options.task, &siderite, options.budget);
    let (parts, omitted) = fit(parts, options.budget, header.len());
    let tail = tail(&omitted, &missing, options.budget);
    let mut markdown = header;
    for part in &parts {
        markdown.push_str(&render(part));
    }
    markdown.push_str(&tail);
    TaskContext {
        task: options.task.clone(),
        siderite,
        docs_version: sources.index.siderite.clone(),
        budget: Budget {
            unit: "bytes",
            limit: options.budget,
            used: markdown.len(),
        },
        parts,
        omitted,
        missing,
        markdown,
    }
}

/// The task's words, without very common ones.
fn task_words(task: &str) -> Vec<String> {
    let words = index::terms(task);
    let common = |w: &String| STOP.split_whitespace().any(|s| s == w);
    let kept = words.into_iter().filter(|w| w.len() > 2);
    kept.filter(|w| !common(w)).collect()
}

/// Lowercase pieces of identifiers and text: `BlogPost` and `blog_posts`
/// both give `blog` and `post` (a final `s` is dropped from longer
/// words).
fn stems(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut word = String::new();
    let mut flush = |word: &mut String| {
        if !word.is_empty() {
            out.insert(stem(word));
            word.clear();
        }
    };
    let mut previous_lower = false;
    for c in text.chars() {
        if !c.is_alphanumeric() {
            flush(&mut word);
            previous_lower = false;
            continue;
        }
        if c.is_uppercase() && previous_lower {
            flush(&mut word);
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
        word.extend(c.to_lowercase());
    }
    flush(&mut word);
    out
}

fn stem(word: &str) -> String {
    match word.strip_suffix('s') {
        Some(base) if base.len() > 2 => base.to_owned(),
        _ => word.to_owned(),
    }
}

/// How many task words occur in `text`.
fn matches(task: &[String], text: &str) -> usize {
    let found = stems(text);
    task.iter().filter(|t| found.contains(&stem(t))).count()
}

fn evidence(snap: &Snapshot) -> String {
    let version = &snap.framework.version;
    format!("compiled app metadata (siderite {version})")
}

fn project(snap: &Snapshot) -> Part {
    let mut text = String::new();
    let app = &snap.app;
    let _ = writeln!(text, "- App: {} {}", app.title, app.version);
    let _ = writeln!(text, "- siderite: {}", snap.framework.version);
    let dbs: Vec<String> = snap
        .databases
        .iter()
        .map(|d| format!("{} ({})", d.alias, d.backend))
        .collect();
    let dbs = if dbs.is_empty() {
        "none configured".to_owned()
    } else {
        dbs.join(", ")
    };
    let _ = writeln!(text, "- Databases: {dbs}");
    let counts = (snap.models.len(), snap.routes.len());
    let _ = writeln!(text, "- Models: {}; routes: {}", counts.0, counts.1);
    Part {
        kind: "project",
        title: "Project".to_owned(),
        source: evidence(snap),
        text,
        truncated: false,
    }
}

/// Name matches count twice as much as field matches.
fn model_score(task: &[String], m: &ModelReport) -> usize {
    let fields: Vec<&str> = m.fields.iter().map(|f| f.name.as_str()).collect();
    let name = format!("{} {}", m.name, m.table);
    2 * matches(task, &name) + matches(task, &fields.join(" "))
}

fn models(snap: &Snapshot, task: &Words, notes: &mut Notes) -> Option<Part> {
    if snap.models.is_empty() {
        return None;
    }
    let ranked = ranked(&snap.models, |m| model_score(task, m));
    if ranked.is_empty() {
        notes.push("no model name or field matched the task".to_owned());
    }
    let mut text = String::new();
    for m in &ranked {
        model(&mut text, m);
    }
    let others: Vec<&str> = snap
        .models
        .iter()
        .filter(|m| !ranked.iter().any(|r| r.name == m.name))
        .map(|m| m.name.as_str())
        .collect();
    if !others.is_empty() {
        let _ = writeln!(text, "Other models: {}.", others.join(", "));
    }
    let total = snap.models.len();
    Some(Part {
        kind: "models",
        title: format!("Models ({} of {total} match the task)", ranked.len()),
        source: evidence(snap),
        text,
        truncated: false,
    })
}

fn route_text(r: &RouteReport) -> String {
    let id = r.operation_id.as_deref().unwrap_or("");
    let summary = r.summary.as_deref().unwrap_or("");
    format!("{} {id} {} {summary}", r.path, r.tags.join(" "))
}

fn routes(snap: &Snapshot, task: &[String]) -> Option<Part> {
    if snap.routes.is_empty() {
        return None;
    }
    let ranked = ranked(&snap.routes, |r| matches(task, &route_text(r)));
    let mut text = String::new();
    if !ranked.is_empty() {
        route_table(&mut text, ranked.iter().copied());
    }
    let others = snap.routes.len() - ranked.len();
    if others > 0 {
        let _ = writeln!(
            text,
            "\n{others} other route(s): `siderite routes` lists them."
        );
    }
    let total = snap.routes.len();
    Some(Part {
        kind: "routes",
        title: format!("Routes ({} of {total} match the task)", ranked.len()),
        source: evidence(snap),
        text,
        truncated: false,
    })
}

/// Items with a positive score, best first (stable for ties).
fn ranked<T>(items: &[T], score: impl Fn(&T) -> usize) -> Vec<&T> {
    let mut scored: Vec<(usize, &T)> = items
        .iter()
        .map(|i| (score(i), i))
        .filter(|(s, _)| *s > 0)
        .collect();
    scored.sort_by_key(|a| std::cmp::Reverse(a.0));
    scored.into_iter().map(|(_, i)| i).collect()
}

fn file_parts(collected: &Collected, notes: &mut Notes) -> Vec<Part> {
    for r in &collected.refused {
        let line = format!("{}: not included ({})", r.path, r.reason);
        notes.push(line);
    }
    collected
        .included
        .iter()
        .map(|file| {
            let lang = Path::new(&file.path)
                .extension()
                .and_then(|e| e.to_str())
                .map_or("", fence_language);
            Part {
                kind: "file",
                title: file.path.clone(),
                source: format!("{} (included with --include)", file.path),
                text: fenced(lang, &file.text),
                truncated: false,
            }
        })
        .collect()
}

fn fence_language(extension: &str) -> &'static str {
    match extension {
        "rs" => "rust",
        "toml" => "toml",
        "json" => "json",
        "sql" => "sql",
        _ => "",
    }
}

fn fenced(lang: &str, text: &str) -> String {
    format!("```{lang}\n{}\n```\n", text.trim_end())
}

fn docs(index: &Index, task: &Words, notes: &mut Notes) -> Vec<Part> {
    let version = &index.siderite;
    let take = |kind: Kind, n: usize| {
        let found = index.search_kind(task, kind);
        found.into_iter().take(n).map(|s| s.entry)
    };
    let mut parts: Vec<Part> = take(Kind::Guide, DOCS)
        .map(|e| Part {
            kind: "docs",
            title: e.title.clone(),
            source: format!("{}:{} (siderite {version} docs)", e.path, e.line),
            text: format!("{}\n", e.text.trim_end()),
            truncated: false,
        })
        .collect();
    if parts.is_empty() {
        notes.push("no documentation section matched the task".to_owned());
    }
    parts.extend(take(Kind::Example, EXAMPLES).map(|e| Part {
        kind: "example",
        title: e.title.clone(),
        source: format!("{} (compiled in siderite {version})", e.path),
        text: fenced("rust", &e.text),
        truncated: false,
    }));
    parts
}

fn header(task: &str, siderite: &str, budget: usize) -> String {
    let task = cell(&index::clip(task, 200));
    format!(
        "# Context for: {task}\n\nsiderite {siderite}; budget {budget} \
         bytes of Markdown; selected locally by keyword, nothing was sent \
         anywhere.\n\n"
    )
}

fn render(part: &Part) -> String {
    let cut = if part.truncated {
        "\n[cut to fit the budget]\n"
    } else {
        ""
    };
    format!(
        "## {}\n\nSource: {}\n\n{}{cut}\n",
        part.title, part.source, part.text
    )
}

/// Take parts in order while they fit; cut the first that does not when
/// enough room is left; list the rest as omitted.
fn fit(parts: Vec<Part>, budget: usize, header: usize) -> Fitted {
    let reserve = TAIL_RESERVE.min(budget / 4);
    let mut room = budget.saturating_sub(header + reserve);
    let mut kept = Vec::new();
    let mut omitted = Vec::new();
    for part in parts {
        let size = render(&part).len();
        if size <= room {
            room -= size;
            kept.push(part);
            continue;
        }
        if room >= MIN_CUT {
            let shorter = cut(part.clone(), room);
            let cut_size = render(&shorter).len();
            // A heading longer than the room cannot be cut to fit.
            if cut_size <= room {
                room -= cut_size;
                kept.push(shorter);
                continue;
            }
        }
        omitted.push(Omitted {
            kind: part.kind,
            title: part.title,
            source: part.source,
            bytes: size,
        });
    }
    (kept, omitted)
}

/// `part` with its text cut at a line boundary so it renders within
/// `room` bytes.
fn cut(mut part: Part, room: usize) -> Part {
    part.truncated = true;
    let frame = render(&Part {
        text: String::new(),
        ..part.clone()
    });
    let allowed = room.saturating_sub(frame.len());
    // Room to close a code block the cut leaves open.
    let allowed = allowed.saturating_sub(FENCE.len());
    let mut text = String::new();
    let mut open = false;
    for line in part.text.lines() {
        if text.len() + line.len() + 1 > allowed {
            break;
        }
        open ^= line.trim_start().starts_with("```");
        text.push_str(line);
        text.push('\n');
    }
    if open {
        text.push_str(FENCE);
    }
    part.text = text;
    part
}

/// The omitted and missing lists, within the reserved bytes.
fn tail(omitted: &[Omitted], missing: &[String], budget: usize) -> String {
    let reserve = TAIL_RESERVE.min(budget / 4);
    let mut lines = Vec::new();
    for o in omitted {
        lines.push(format!("- omitted ({} bytes): {}", o.bytes, o.title));
    }
    for m in missing {
        lines.push(format!("- missing: {m}"));
    }
    let mut out = String::from("## Not included\n\n");
    for line in lines {
        let line = index::clip(&line, 160) + "\n";
        let more = "- ...\n";
        if out.len() + line.len() + more.len() > reserve {
            out.push_str(more);
            break;
        }
        out.push_str(&line);
    }
    out
}
