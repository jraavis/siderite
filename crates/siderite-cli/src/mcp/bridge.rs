//! The application bridge: the selected app's own metadata, from its
//! `ai dump-context --format json`, built only when a project tool is
//! first called and rebuilt when the project's files change.
//!
//! The app runs with stdin closed; its stdout and stderr go to this
//! process's stderr, so nothing it prints reaches the protocol stream.
//! Its report travels through a private file. One build runs at a time.
//! Cargo leads its own process group (Unix), and a call that is
//! cancelled or times out kills the whole group: cargo, rustc and the
//! app it started.

use crate::process::{self, Group, KillOnDrop};
use crate::project::ReportFile;
use crate::report::channel::REPORT_PATH_ENV;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Most files looked at for the fingerprint; past it, every call
/// rebuilds.
const MAX_FILES: usize = 20_000;
/// Directories never part of the fingerprint.
const SKIPPED: [&str; 4] = ["target", ".git", ".siderite", "node_modules"];

/// The app's context and its freshness, or why there is none.
pub(crate) type Snapshot = Result<(Arc<Value>, Freshness), String>;

/// How a snapshot relates to the files on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Freshness {
    /// Built earlier; no project file changed since.
    Cached,
    /// Built for this call.
    Rebuilt,
    /// Project files changed while it was being built.
    Stale,
}

struct Cached {
    fingerprint: Option<u64>,
    context: Arc<Value>,
}

/// The selected application and its last snapshot.
pub(crate) struct Bridge {
    dir: PathBuf,
    bin: Option<String>,
    cargo: PathBuf,
    cache: Mutex<Option<Cached>>,
}

impl Bridge {
    /// The application package in `dir`, binary `bin` when named.
    pub(crate) fn new(dir: PathBuf, bin: Option<String>) -> Self {
        Self {
            dir,
            bin,
            cargo: PathBuf::from("cargo"),
            cache: Mutex::new(None),
        }
    }

    /// The same bridge running `cargo` from `program`.
    #[cfg(test)]
    pub(crate) fn with_cargo(mut self, program: PathBuf) -> Self {
        self.cargo = program;
        self
    }

    /// The app's context (`ai dump-context` data) and how fresh it is.
    ///
    /// Errors:
    ///   Why the app could not produce it (build failure, crash).
    pub(crate) async fn context(&self) -> Snapshot {
        let mut cache = self.cache.lock().await;
        let before = fingerprint(&self.dir);
        if let Some(cached) = cache.as_ref()
            && before.is_some()
            && cached.fingerprint == before
        {
            return Ok((Arc::clone(&cached.context), Freshness::Cached));
        }
        let context = Arc::new(self.dump().await?);
        let after = fingerprint(&self.dir);
        *cache = Some(Cached {
            fingerprint: before,
            context: Arc::clone(&context),
        });
        let freshness = if after == before {
            Freshness::Rebuilt
        } else {
            Freshness::Stale
        };
        Ok((context, freshness))
    }

    async fn dump(&self) -> Result<Value, String> {
        let args = ["ai", "dump-context", "--format", "json"];
        let envelope = self.report(&args).await?;
        envelope
            .get("data")
            .cloned()
            .ok_or_else(|| "the app's report has no data".to_owned())
    }

    /// Run the app with `args` and return the JSON report it writes.
    ///
    /// Errors:
    ///   Why the app wrote no report (build failure, crash).
    pub(crate) async fn report(&self, args: &[&str]) -> Result<Value, String> {
        let report = ReportFile::new().map_err(|e| e.to_string())?;
        let mut command = tokio::process::Command::new(&self.cargo);
        command.arg("run").arg("--quiet").current_dir(&self.dir);
        if let Some(bin) = &self.bin {
            command.arg("--bin").arg(bin);
        }
        command
            .arg("--")
            .args(args)
            .env(REPORT_PATH_ENV, report.path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(std::io::stderr()))
            .stderr(Stdio::from(std::io::stderr()));
        process::own(&mut command);
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot run cargo: {e}"))?;
        // Dropping this future (cancel, timeout) kills the whole group.
        let guard = child.id().map(|pid| KillOnDrop::new(Group::led_by(pid)));
        let status = child.wait().await;
        // Nothing cargo or the app started may outlive the call.
        drop(guard);
        let status = status.map_err(|e| format!("cannot run cargo: {e}"))?;
        let Some(text) = report.read(true) else {
            return Err(format!(
                "the app exited with {status} without its report (build \
                 errors are on the server's stderr)"
            ));
        };
        let envelope = serde_json::from_str::<Value>(&text);
        envelope.map_err(|e| e.to_string())
    }
}

/// A hash of the paths, sizes and modification times of the project's
/// source files and its nearest `Cargo.lock`; `None` when there are too
/// many files to look at.
pub(crate) fn fingerprint(dir: &Path) -> Option<u64> {
    let mut entries = Vec::new();
    walk(dir, &mut entries)?;
    let lock = dir
        .ancestors()
        .map(|d| d.join("Cargo.lock"))
        .find(|p| p.is_file());
    entries.extend(lock.and_then(|p| stamp(&p)));
    entries.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for entry in entries {
        for byte in entry.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    Some(hash)
}

fn stamp(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    let nanos = since.as_nanos();
    Some(format!("{}\t{}\t{nanos}", path.display(), meta.len()))
}

fn walk(dir: &Path, out: &mut Vec<String>) -> Option<()> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if !SKIPPED.contains(&name.as_ref()) {
                walk(&path, out)?;
            }
        } else if kind.is_file() {
            out.extend(stamp(&path));
            if out.len() > MAX_FILES {
                return None;
            }
        }
    }
    Some(())
}
