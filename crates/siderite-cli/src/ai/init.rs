//! `ai init`: write or refresh `.siderite/context.md`, the one context
//! file `siderite` owns in a project.
//!
//! The file holds a generated block between two markers: commands,
//! conventions, a summary of this project (from the snapshot) and
//! examples for this framework version that are compiled in the
//! framework's own tests. Everything outside the markers belongs to the
//! user and is kept byte for byte. The begin marker records the format,
//! the framework version and a hash of the block, so a rerun can tell an
//! up-to-date file from a stale one (shown as a diff) and from one whose
//! block was edited by hand (left alone unless `--force`). Nothing is
//! fetched from the network and no source files are copied.

use super::{links, markdown};
use crate::error::CliError;
use crate::generate::diff::unified;
use crate::generate::{Plan, preview, summary};
use crate::report::snapshot::Snapshot;
use std::fmt::Write;
use std::path::Path;

/// The context file, relative to the package directory.
pub(crate) const PATH: &str = ".siderite/context.md";

/// Version of the block layout, bumped when it changes.
const FORMAT: u32 = 1;
const BEGIN: &str = "<!-- siderite:context begin";
const END: &str = "<!-- siderite:context end -->";
const VERSION: &str = env!("CARGO_PKG_VERSION");

const ROUTE: &str = include_str!("examples/route.rs");
const MODEL: &str = include_str!("examples/model.rs");
const MAIN: &str = include_str!("examples/main.rs");

type Code = Result<u8, CliError>;

/// What `ai init` asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct InitOptions {
    /// Report whether the file is current; write nothing.
    pub check: bool,
    /// Show what would change; write nothing.
    pub dry_run: bool,
    /// Replace a hand-edited generated block.
    pub force: bool,
}

/// `ai init` arguments: the flags and the `--link` files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Init {
    /// The flags.
    pub options: InitOptions,
    /// Instruction files to link, relative to the repository root.
    pub links: Vec<String>,
}

/// The state of the context file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum State {
    /// There is no file yet.
    Missing,
    /// The block matches what would be generated now.
    Current,
    /// The block is from another version or an older project state.
    Stale(String),
    /// The block was edited by hand.
    Edited,
}

/// Parse `ai init` flags (`rest` starts with `ai init`).
///
/// Errors:
///   A usage error for anything else.
pub(crate) fn parse(rest: &[String]) -> Result<Init, CliError> {
    let mut init = Init::default();
    let options = &mut init.options;
    let mut args = rest[2..].iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => options.check = true,
            "--dry-run" => options.dry_run = true,
            "--force" => options.force = true,
            "--link" => {
                let file = args.next().ok_or_else(|| usage(arg))?;
                init.links.push(links::checked(file)?);
            }
            other => return Err(usage(other)),
        }
    }
    Ok(init)
}

fn usage(arg: &str) -> CliError {
    CliError::usage(format!(
        "unexpected argument `{arg}`; usage: ai init [--check] \
         [--dry-run] [--force] [--link FILE]..."
    ))
}

/// Create or refresh the context file under `dir` for `snap`.
///
/// Returns:
///   The exit code: `0` when the file is (now) current, `1` for
///   `--check` on a missing or stale file.
///
/// Errors:
///   [`CliError::Conflict`] when the file is not ours or its block was
///   edited (without `--force`), or a write error.
pub(crate) fn run(dir: &Path, snap: &Snapshot, init: &Init) -> Code {
    let o = init.options;
    let body = block(snap);
    let existing = std::fs::read_to_string(dir.join(PATH)).ok();
    let parts = existing.as_deref().map(split).transpose()?;
    let state = state(parts.as_ref(), &body);
    let text = match &parts {
        Some(p) => format!("{}{}{}", p.before, wrap(&body), p.after),
        None => format!("{}\n{NOTES}", wrap(&body)),
    };
    if state == State::Edited && !o.force && !o.check {
        let old = parts.as_ref().map_or("", |p| p.body.as_str());
        let diff = unified(PATH, old, &body);
        return Err(CliError::Conflict(format!(
            "{PATH}: the generated block was edited by hand; keep your \
             notes outside the markers, or pass --force to replace the \
             block (your text outside it is kept)\n{diff}"
        )));
    }
    let mut plan = Plan::new(dir);
    plan.add_owned(PATH, text)?;
    let (links, stale_links) = link_plan(dir, &init.links)?;
    report(&state);
    if o.check {
        let stale = state != State::Current || !stale_links.is_empty();
        return Ok(u8::from(stale));
    }
    if o.dry_run {
        print!("{}", preview(&plan.check()?));
        print!("{}", preview(&links.check()?));
        return Ok(0);
    }
    let mut changes = plan.apply()?;
    changes.extend(links.apply()?);
    print!("{}", summary(&changes));
    Ok(0)
}

type LinkPlan = Result<(Plan, Vec<String>), CliError>;

/// The link blocks for `files` (relative to the repository root), and
/// the files among them that would change.
///
/// Errors:
///   `ai.E002` for a file whose markers cannot be updated safely.
pub(super) fn link_plan(dir: &Path, files: &[String]) -> LinkPlan {
    let repo = crate::generate::ci::repository(dir);
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.into());
    let package = canonical(dir);
    let rel = package
        .strip_prefix(canonical(&repo))
        .unwrap_or(Path::new(""));
    let rel = rel.to_string_lossy().replace('\\', "/");
    let context = if rel.is_empty() {
        PATH.to_owned()
    } else {
        format!("{rel}/{PATH}")
    };
    let mut plan = Plan::new(&repo);
    let mut stale = Vec::new();
    for file in files {
        let existing = std::fs::read_to_string(repo.join(file)).ok();
        let block = links::block(file, &context);
        let merged = links::merge(file, existing.as_deref(), &block)?;
        let changed = existing.as_deref() != Some(merged.as_str());
        let state = if changed {
            stale.push(file.clone());
            "will be updated"
        } else {
            "up to date"
        };
        eprintln!("{file}: link {state}");
        plan.add_owned(file, merged)?;
    }
    Ok((plan, stale))
}

const NOTES: &str = "\
## Project notes

Your notes for AI tools go here. `siderite ai init` refreshes only the
block between the markers above and keeps the rest of this file.
";

fn report(state: &State) {
    let line = match state {
        State::Missing => format!("{PATH}: creating"),
        State::Current => format!("{PATH}: up to date"),
        State::Stale(why) => format!("{PATH}: stale ({why})"),
        State::Edited => format!("{PATH}: the generated block was edited"),
    };
    eprintln!("{line}");
}

/// An existing file, split around its block.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Parts {
    before: String,
    /// `format=.. siderite=.. hash=..` attributes of the begin marker.
    attrs: String,
    body: String,
    after: String,
}

/// Split `text` around the markers.
fn split(text: &str) -> Result<Parts, CliError> {
    let foreign = || {
        CliError::Conflict(format!(
            "{PATH} exists but has no siderite markers; it was not written \
             by `siderite ai init`, so it is left alone (move it away to \
             regenerate it)"
        ))
    };
    let start = text.find(BEGIN).ok_or_else(foreign)?;
    let marker_end = text[start..].find("-->").ok_or_else(foreign)? + start;
    let end = text[marker_end..].find(END).ok_or_else(foreign)? + marker_end;
    let attrs = text[start + BEGIN.len()..marker_end].trim().to_owned();
    let body_start = text[marker_end..]
        .find('\n')
        .map_or(text.len(), |i| marker_end + i + 1);
    let after_start = end + END.len();
    let after_start = text[after_start..]
        .strip_prefix('\n')
        .map_or(after_start, |_| after_start + 1);
    Ok(Parts {
        before: text[..start].to_owned(),
        attrs,
        body: text[body_start.min(end)..end].to_owned(),
        after: text[after_start..].to_owned(),
    })
}

fn state(parts: Option<&Parts>, body: &str) -> State {
    let Some(parts) = parts else {
        return State::Missing;
    };
    let stored = attr(&parts.attrs, "hash");
    if stored != Some(hash(&parts.body).as_str()) {
        return State::Edited;
    }
    if parts.body == body && parts.attrs == attrs(body) {
        return State::Current;
    }
    let version = attr(&parts.attrs, "siderite").unwrap_or("unknown");
    if version != VERSION {
        let why = format!("written for siderite {version}, now {VERSION}");
        return State::Stale(why);
    }
    State::Stale("the project or the template changed".to_owned())
}

fn attr<'a>(attrs: &'a str, key: &str) -> Option<&'a str> {
    attrs
        .split_whitespace()
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
}

fn attrs(body: &str) -> String {
    format!("format={FORMAT} siderite={VERSION} hash={}", hash(body))
}

fn wrap(body: &str) -> String {
    format!("{BEGIN} {} -->\n{body}{END}\n", attrs(body))
}

/// FNV-1a, 64 bits: stable across platforms and releases.
fn hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        h ^= u64::from(byte);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The generated block for `snap`.
fn block(snap: &Snapshot) -> String {
    let mut out = String::new();
    let title = &snap.app.title;
    let _ = writeln!(out, "# Siderite context: {title}\n");
    let _ = writeln!(
        out,
        "Generated by `siderite ai init` for siderite {VERSION}. Run it \
         again after changing models or routes; edit only outside this \
         block.\n"
    );
    out.push_str(GUIDE);
    markdown::project(&mut out, snap);
    examples(&mut out);
    out
}

const GUIDE: &str = "\
## Commands

- `siderite dev`: rebuild and restart on changes.
- `siderite check --json`: framework checks (models, migrations, routes);
  `siderite lint` is Clippy, a different thing.
- `siderite routes --json`, `siderite openapi export`: the API surface.
- `siderite makemigrations`, then `siderite migrate`: schema changes.
  Never edit a migration that has been applied.
- `siderite ai dump-context`: this project's current metadata.

## Conventions

- Handlers use the route attributes (`#[get(\"/path/{id}\")]`, `#[post]`,
  ...) and are registered explicitly with `App::routes(routes![...])`.
- Request bodies derive `Deserialize, Validate, Schema`; `#[field(...)]`
  constraints reject bad input with 422 before the handler runs.
- Errors are `ApiError` values (`ApiError::not_found(..)`, ...).
- Models derive `Model` with `#[model(table = \"...\")]` and are queried
  with `Model::objects(&db)`; prefer one query over per-row queries.
- Register models with `AppCli::models` so checks and `makemigrations`
  see them.

";

fn examples(out: &mut String) {
    let _ = writeln!(
        out,
        "## Examples (siderite {VERSION})\n\nThese compile against \
         siderite {VERSION}; they are checked in its test suite.\n"
    );
    for (title, code) in [
        ("A route with validation", ROUTE),
        ("A model and a query", MODEL),
        ("The command line", MAIN),
    ] {
        let _ = writeln!(out, "### {title}\n\n```rust\n{code}```\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_and_hashes() {
        assert_eq!(hash(""), "cbf29ce484222325");
        assert_ne!(hash("a"), hash("b"));
        let attrs = "format=1 siderite=0.1.0 hash=abc";
        assert_eq!(attr(attrs, "siderite"), Some("0.1.0"));
        assert_eq!(attr(attrs, "nope"), None);
    }

    #[test]
    fn split_keeps_text_outside_the_markers() {
        let text = format!("intro\n{}notes\n", wrap("body\n"));
        let parts = split(&text).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parts.before, "intro\n");
        assert_eq!(parts.body, "body\n");
        assert_eq!(parts.after, "notes\n");
        assert!(split("no markers").is_err());
    }
}
