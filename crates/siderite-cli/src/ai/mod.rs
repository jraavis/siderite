//! `ai dump-context`: the application snapshot as context for AI tools.
//!
//! Offline by default: the context comes from compiled metadata and no
//! database is touched. Migration status is labelled `unknown` unless
//! `--live` asks for it, in which case it is read exactly as
//! `showmigrations --json` reads it (bounded, read-only, redacted). The
//! context names the binary it came from and when that binary was built,
//! so a tool can tell when it may be stale.

mod files;
pub(crate) mod init;
mod links;
mod markdown;
pub(crate) mod task;
pub(crate) mod validate;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_init;
#[cfg(test)]
mod tests_task;
#[cfg(test)]
mod tests_validate;

use crate::error::CliError;
use crate::report::snapshot::{Limits, Snapshot, snapshot};
use crate::report::{Diagnostic, Envelope, MigrationStatusData, Severity};
use crate::settings::CliSettings;
use crate::status::{STATUS_TIMEOUT, Selection, status};
use serde::{Deserialize, Serialize};
use siderite_core::App;
use siderite_orm::ModelMeta;
use std::path::Path;

pub(crate) use markdown::render as render_markdown;

/// How the context is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    /// Markdown for people and prompts (the default).
    Markdown,
    /// The versioned JSON envelope.
    Json,
}

/// Parsed `ai dump-context` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    /// Output format.
    pub format: Format,
    /// Read the live migration status.
    pub live: bool,
    /// Size bounds of the snapshot.
    pub limits: Limits,
}

const USAGE: &str = "usage: ai dump-context [--format markdown|json] \
     [--live] [--max-bytes N]";

/// Parse `rest` (`["ai", "dump-context", ...]`); `json` is the global
/// `--json` flag.
///
/// Errors:
///   A usage error for anything else.
pub(crate) fn parse(rest: &[String], json: bool) -> Result<Options, CliError> {
    if rest.get(1).map(String::as_str) != Some("dump-context") {
        let init = "ai init [--check] [--dry-run] [--force]";
        let task = "ai context --task TEXT [--budget BYTES] [--include PATH]";
        let msg = format!("{USAGE}\n       {init}\n       {task}");
        return Err(CliError::usage(msg));
    }
    let mut options = Options {
        format: if json { Format::Json } else { Format::Markdown },
        live: false,
        limits: Limits::default(),
    };
    let mut args = rest[2..].iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| args.next().cloned());
        match flag {
            "--live" => options.live = true,
            "--format" => options.format = format(value().as_deref())?,
            "--max-bytes" => options.limits.max_bytes = bytes(value())?,
            _ => {
                let msg = format!("unexpected argument `{arg}`; {USAGE}");
                return Err(CliError::usage(msg));
            }
        }
    }
    Ok(options)
}

fn format(value: Option<&str>) -> Result<Format, CliError> {
    match value {
        Some("markdown" | "md") => Ok(Format::Markdown),
        Some("json") => Ok(Format::Json),
        _ => Err(CliError::usage("--format is markdown or json")),
    }
}

fn bytes(value: Option<String>) -> Result<usize, CliError> {
    match value.as_deref().map(str::parse::<usize>) {
        Some(Ok(n)) if n > 0 => Ok(n),
        _ => Err(CliError::usage("--max-bytes needs a positive integer")),
    }
}

/// Where the context came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// File name of the app binary.
    pub binary: Option<String>,
    /// When the binary was built (modified), in seconds since the Unix
    /// epoch; the context is as old as this.
    pub built_unix: Option<u64>,
}

/// What is known about migrations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Migrations {
    /// `unknown` (offline), `read` or `failed`.
    pub state: String,
    /// Why it is unknown or failed.
    pub detail: Option<String>,
    /// The status, when read.
    pub status: Option<MigrationStatusData>,
}

/// The `data` of an `ai dump-context` envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Context {
    /// The application snapshot.
    pub snapshot: Snapshot,
    /// The binary it came from.
    pub source: Source,
    /// Migration status, labelled.
    pub migrations: Migrations,
}

/// The `ai dump-context` envelope.
pub(crate) type Report = Envelope<Context>;

/// What the context is built from.
pub(crate) struct Inputs<'a> {
    /// A freshly built app.
    pub app: &'a App,
    /// Registered models.
    pub models: &'a [&'static ModelMeta],
    /// CLI settings (aliases only reach the output).
    pub settings: &'a CliSettings,
    /// The database to read when live, if one is configured.
    pub database: Option<Selection>,
    /// The migrations directory.
    pub migrations_dir: &'a Path,
}

/// Build the context envelope.
pub(crate) async fn context(inputs: Inputs<'_>, options: &Options) -> Report {
    let Inputs { app, models, .. } = inputs;
    let snap = snapshot(app, models, inputs.settings, options.limits);
    let mut diagnostics = snap.diagnostics;
    let migrations = if options.live {
        live(inputs.database, inputs.migrations_dir, &mut diagnostics).await
    } else {
        Migrations {
            state: "unknown".to_owned(),
            detail: Some("offline; pass --live to read the database".into()),
            status: None,
        }
    };
    let data = Context {
        snapshot: snap.data,
        source: source(),
        migrations,
    };
    Envelope::new("ai dump-context", data, diagnostics)
}

async fn live(
    database: Option<Selection>,
    dir: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> Migrations {
    let read = match database {
        Some(selection) => status(&selection, dir, STATUS_TIMEOUT).await,
        None => Err(CliError::NoDatabase {
            alias: "default".to_owned(),
        }),
    };
    match read {
        Ok(report) => Migrations {
            state: "read".to_owned(),
            detail: None,
            status: Some(report.data),
        },
        Err(err) => {
            let hint = "check the database settings, or drop --live";
            let message = err.to_string();
            let diag = Diagnostic::new("ai.E001", Severity::Error, &message);
            let diag = diag.with_hint(hint);
            let detail = diag.message().to_owned();
            diagnostics.push(diag);
            Migrations {
                state: "failed".to_owned(),
                detail: Some(detail),
                status: None,
            }
        }
    }
}

/// The running binary's name and modification time.
fn source() -> Source {
    let exe = std::env::current_exe().ok();
    let binary = exe
        .as_ref()
        .and_then(|e| e.file_name())
        .map(|n| n.to_string_lossy().into_owned());
    let built_unix = exe
        .and_then(|e| e.metadata().ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    Source { binary, built_unix }
}
