//! `doctor --live`: opt-in, read-only probes of configured databases.
//!
//! Each requested alias is connected and its migration status read
//! through the same code as `showmigrations --json`, within a per-alias
//! timeout and with at most [`CONCURRENCY`] probes in flight. Nothing is
//! written: SQLite files are opened read-only and never created, and the
//! history table is only read. Connecting can still run driver session
//! setup (PostgreSQL and MySQL pools set session options such as the time
//! zone); that state ends with the connection.

use super::Findings;
use crate::connect::{backend_kind, sqlite_path};
use crate::report::UrlSource;
use crate::status::{Selection, status};
use siderite_config::Settings;
use siderite_orm::BackendKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::task::JoinSet;

/// Most probes in flight at once.
pub(crate) const CONCURRENCY: usize = 4;

/// Default bound on one alias probe.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// What `--live` asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Live {
    /// `--database ALIAS` (repeatable); empty means every alias.
    pub aliases: Vec<String>,
    /// `--timeout-ms`, per alias.
    pub timeout: Duration,
    /// `--migrations-dir`, relative to the project directory.
    pub migrations_dir: PathBuf,
}

type Out<'a> = &'a mut Findings;

/// One probe to run.
struct Target {
    alias: String,
    url: String,
}

/// Probe the requested aliases of `settings` and add one `live.ALIAS`
/// check per alias, in request order.
pub(crate) async fn probe(s: &Settings, live: &Live, dir: &Path, f: Out<'_>) {
    let requested = requested(s, live);
    let mut targets = Vec::new();
    for alias in &requested {
        if let Some(target) = prepare(s, alias, dir, f) {
            targets.push(target);
        }
    }
    let migrations = dir.join(&live.migrations_dir);
    for chunk in targets.chunks(CONCURRENCY) {
        let mut set = JoinSet::new();
        for (i, target) in chunk.iter().enumerate() {
            let selection = Selection {
                url: target.url.clone(),
                alias: Some(target.alias.clone()),
                source: UrlSource::Settings,
            };
            let (dir, timeout) = (migrations.clone(), live.timeout);
            set.spawn(async move {
                let started = Instant::now();
                let outcome = status(&selection, &dir, timeout).await;
                (i, outcome, started.elapsed())
            });
        }
        let mut done = set.join_all().await;
        done.sort_by_key(|(i, ..)| *i);
        for (i, outcome, elapsed) in done {
            let name = format!("live.{}", chunk[i].alias);
            match outcome {
                Ok(report) => {
                    let data = report.data;
                    let ms = elapsed.as_millis();
                    let detail = format!(
                        "connected in {ms} ms; {} applied, {} pending",
                        data.applied, data.pending
                    );
                    f.ok(&name, &detail);
                }
                Err(err) => {
                    let hint = "check that the server is up and reachable, \
                                the credentials, and --timeout-ms";
                    f.error(&name, "doctor.E007", (&err.to_string(), hint));
                }
            }
        }
    }
}

/// The aliases to probe: those named, else every configured one.
fn requested(settings: &Settings, live: &Live) -> Vec<String> {
    if live.aliases.is_empty() {
        return settings.databases.keys().cloned().collect();
    }
    let mut seen = Vec::new();
    for alias in &live.aliases {
        if !seen.contains(alias) {
            seen.push(alias.clone());
        }
    }
    seen
}

/// The URL to connect for `alias`, or why it is not probed.
fn prepare(s: &Settings, alias: &str, dir: &Path, f: Out) -> Option<Target> {
    let name = format!("live.{alias}");
    let Some(db) = s.databases.get(alias) else {
        let detail = format!("alias `{alias}` is not configured");
        let hint = "configure it in siderite.toml or SIDERITE_DATABASES__*";
        f.error(&name, "doctor.E008", (&detail, hint));
        return None;
    };
    let url = db.url.expose();
    let target = |url: String| Target {
        alias: alias.to_owned(),
        url,
    };
    match backend_kind(url) {
        Some(BackendKind::Sqlite) => sqlite(&name, url, dir, f).map(target),
        Some(BackendKind::Postgres | BackendKind::MySql) => {
            // A network server: probed with its configured URL.
            Some(target(url.clone()))
        }
        Some(kind) => {
            let detail = format!("{kind:?} store: not probed by doctor");
            f.info(&name, &detail);
            None
        }
        None => {
            let detail = "unsupported URL scheme: not probed";
            f.info(&name, detail);
            None
        }
    }
}

/// A read-only URL for an existing SQLite file; a relative path is
/// resolved against the project directory, where the app runs.
fn sqlite(name: &str, url: &str, dir: &Path, f: Out<'_>) -> Option<String> {
    let Some(path) = sqlite_path(url) else {
        f.info(name, "in-memory SQLite: nothing to probe");
        return None;
    };
    let file = dir.join(path);
    if !file.is_file() {
        let detail = format!("{path} is missing (doctor never creates it)");
        let hint = "run `siderite migrate` or fix the path";
        f.error(name, "doctor.E007", (&detail, hint));
        return None;
    }
    Some(format!("sqlite://{}?mode=ro", file.display()))
}
