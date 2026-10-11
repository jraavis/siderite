//! Source files a user explicitly adds to a task context
//! (`--include PATH`).
//!
//! Only paths inside the project root are read. Files git ignores are
//! skipped (through `git ls-files --exclude-standard`; without git, build
//! output and hidden directories are skipped instead), and files whose
//! names suggest secrets are refused even when tracked. Contents are
//! redacted like every other output.

use crate::report::redact_text;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Files larger than this are refused.
const MAX_FILE_BYTES: u64 = 1 << 20;
/// At most this many files per context.
const MAX_FILES: usize = 200;
/// Directories skipped when git is not available.
const SKIPPED_DIRS: &[&str] = &["target", "node_modules"];

/// A file to include.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Included {
    /// Path relative to the root, with `/` separators.
    pub path: String,
    /// Its text, redacted.
    pub text: String,
}

/// A path that was not included, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refused {
    /// The path as given (or relative to the root).
    pub path: String,
    /// Why.
    pub reason: &'static str,
}

/// The result of collecting `--include` paths.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Collected {
    /// Files read, sorted by path.
    pub included: Vec<Included>,
    /// Paths left out.
    pub refused: Vec<Refused>,
}

/// Read the files under `requested` that lie inside `root`.
pub(crate) fn collect(root: &Path, requested: &[PathBuf]) -> Collected {
    let mut out = Collected::default();
    let Ok(root) = root.canonicalize() else {
        return out;
    };
    let mut files = Vec::new();
    for path in requested {
        // Paths inside the root are shown relative to it.
        let joined = root.join(path);
        let inside = joined.strip_prefix(&root).unwrap_or(path);
        let shown = inside.display().to_string();
        let refuse = |reason| Refused {
            path: shown.clone(),
            reason,
        };
        let Ok(path) = joined.canonicalize() else {
            out.refused.push(refuse("not found"));
            continue;
        };
        if !path.starts_with(&root) {
            out.refused.push(refuse("outside the project"));
            continue;
        }
        match listed(&root, &path) {
            Some(found) if found.is_empty() => {
                out.refused.push(refuse("ignored by git"));
            }
            Some(found) => files.extend(found),
            None => files.extend(walk(&path)),
        }
    }
    files.sort();
    files.dedup();
    for file in files {
        if out.included.len() == MAX_FILES {
            let rel = relative(&root, &file);
            out.refused.push(Refused {
                path: rel,
                reason: "over the file limit",
            });
            continue;
        }
        match read(&root, &file) {
            Ok(included) => out.included.push(included),
            Err(refused) => out.refused.push(refused),
        }
    }
    out
}

fn relative(root: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(root).unwrap_or(file);
    rel.to_string_lossy().replace('\\', "/")
}

fn read(root: &Path, file: &Path) -> Result<Included, Refused> {
    let path = relative(root, file);
    let refuse = |reason| Refused {
        path: path.clone(),
        reason,
    };
    let name = file.file_name().map(|n| n.to_string_lossy());
    if name.as_deref().is_some_and(is_secret_name) {
        return Err(refuse("may hold secrets"));
    }
    let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    if size > MAX_FILE_BYTES {
        return Err(refuse("larger than 1 MiB"));
    }
    let bytes = std::fs::read(file).map_err(|_| refuse("unreadable"))?;
    let text = String::from_utf8(bytes).map_err(|_| refuse("not text"))?;
    Ok(Included {
        text: redact_text(&text),
        path,
    })
}

/// Names of files that usually hold credentials.
pub(crate) fn is_secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let secret_ext = [".pem", ".key", ".p12", ".pfx", ".keystore"];
    lower == ".env"
        || lower.starts_with(".env.")
        || lower.starts_with("id_rsa")
        || lower.starts_with("id_ed25519")
        || lower.ends_with(".local.toml")
        || secret_ext.iter().any(|e| lower.ends_with(e))
}

/// The files git would not ignore under `path`; `None` when git cannot
/// tell (not installed, or not a repository).
fn listed(root: &Path, path: &Path) -> Option<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["ls-files", "-z", "--cached", "--others"])
        .args(["--exclude-standard", "--"])
        .arg(path)
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // Paths are relative to the directory git ran in.
    let names = output.stdout.split(|b| *b == 0).filter(|n| !n.is_empty());
    let files = names
        .map(|n| root.join(String::from_utf8_lossy(n).as_ref()))
        .filter(|f| f.is_file())
        .collect();
    Some(files)
}

/// Every file under `path` (or `path` itself), skipping hidden and build
/// directories.
fn walk(path: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(next) = stack.pop() {
        if next.is_file() {
            found.push(next);
            continue;
        }
        let Ok(listing) = std::fs::read_dir(&next) else {
            continue;
        };
        for item in listing.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            let skip = name.starts_with('.') && item.path().is_dir();
            if skip || SKIPPED_DIRS.contains(&name.as_str()) {
                continue;
            }
            stack.push(item.path());
        }
    }
    found
}
