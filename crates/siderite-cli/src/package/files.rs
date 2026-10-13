//! What goes into a package: declared assets and migrations, minus what
//! must never ship (credentials, databases, build caches, VCS data).

use crate::error::CliError;
use crate::fsio::io_error;
use crate::report::redact_text;
use crate::secret_files::is_secret_name;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

/// Names never packaged, wherever they appear, besides those
/// [`is_secret_name`] refuses. A trailing `*` matches any suffix, a
/// leading `*` any prefix.
const EXCLUDED: [&str; 10] = [
    ".git*",
    ".env*",
    "*.db",
    "*.db-journal",
    "*.db-wal",
    "*.db-shm",
    "*.sqlite",
    "*.sqlite3",
    "target",
    ".siderite",
];

/// Text files up to this size are checked for credentials.
const SCANNED_BYTES: u64 = 1024 * 1024;

fn excluded(name: &str) -> bool {
    is_secret_name(name) || EXCLUDED.iter().any(|p| matches(p, name))
}

fn matches(pattern: &str, name: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        name.starts_with(prefix)
    } else if let Some(suffix) = pattern.strip_prefix('*') {
        name.ends_with(suffix)
    } else {
        name == pattern
    }
}

/// A file to package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Source {
    /// Path inside the package, with `/` separators.
    pub rel: String,
    /// The file on disk.
    pub path: PathBuf,
}

/// The files under the declared paths, and the paths left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Selection {
    /// Files to copy, sorted by package path.
    pub files: Vec<Source>,
    /// Package-relative paths skipped by the exclusion rules.
    pub excluded: Vec<String>,
}

type Selected = Result<Selection, CliError>;

/// A declared path: relative, inside the package, no `..`.
fn checked(declared: &str) -> Result<&Path, CliError> {
    let path = Path::new(declared);
    let inside = path.components().all(|c| matches!(c, Component::Normal(_)));
    if inside && !declared.is_empty() {
        return Ok(path);
    }
    Err(CliError::usage(format!(
        "package path `{declared}` must be relative to the package and \
         stay inside it"
    )))
}

/// Collect `declared` paths (files or directories) under `root`.
///
/// Errors:
///   A usage error for a path outside the package, a declared path that
///   does not exist, or a symbolic link; an IO error otherwise.
pub(crate) fn select(root: &Path, declared: &[String]) -> Selected {
    let mut selection = Selection::default();
    for item in declared {
        let rel = checked(item)?;
        let path = root.join(rel);
        if !path.exists() {
            let msg = format!("declared package path `{item}` does not exist");
            return Err(CliError::usage(msg));
        }
        walk(&path, &rel_string(rel), &mut selection)?;
    }
    selection.files.sort_by(|a, b| a.rel.cmp(&b.rel));
    selection.files.dedup_by(|a, b| a.rel == b.rel);
    selection.excluded.sort();
    selection.excluded.dedup();
    Ok(selection)
}

fn rel_string(path: &Path) -> String {
    let parts = path.components().map(|c| c.as_os_str().to_string_lossy());
    parts.collect::<Vec<_>>().join("/")
}

fn walk(path: &Path, rel: &str, out: &mut Selection) -> Result<(), CliError> {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if excluded(name) {
        out.excluded.push(rel.to_owned());
        return Ok(());
    }
    let meta = std::fs::symlink_metadata(path);
    let meta = meta.map_err(|err| io_error("read", path, &err))?;
    if meta.file_type().is_symlink() {
        let msg = format!("`{rel}` is a symbolic link; package real files");
        return Err(CliError::usage(msg));
    }
    if meta.is_file() {
        out.files.push(Source {
            rel: rel.to_owned(),
            path: path.to_path_buf(),
        });
        return Ok(());
    }
    let entries = std::fs::read_dir(path);
    let entries = entries.map_err(|err| io_error("list", path, &err))?;
    for entry in entries {
        let entry = entry.map_err(|err| io_error("list", path, &err))?;
        let child = entry.file_name().to_string_lossy().into_owned();
        walk(&entry.path(), &format!("{rel}/{child}"), out)?;
    }
    Ok(())
}

/// The first selected text file that holds something that looks like a
/// credential (a URL password, a secret-looking `key=value`).
pub(crate) fn credentials(files: &[Source]) -> Option<String> {
    files.iter().find_map(|file| {
        let size = std::fs::metadata(&file.path).map(|m| m.len());
        if size.map_or(true, |n| n > SCANNED_BYTES) {
            return None;
        }
        let text = std::fs::read_to_string(&file.path).ok()?;
        (redact_text(&text) != text).then(|| file.rel.clone())
    })
}

/// One packaged file in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Entry {
    /// Path inside the package.
    pub path: String,
    /// Size in bytes.
    pub bytes: u64,
    /// SHA-256, lowercase hexadecimal.
    pub sha256: String,
}

type Copied = Result<Entry, CliError>;

/// Copy `from` to `to` (creating directories) and describe the copy.
///
/// Errors:
///   An IO error.
pub(crate) fn copy(from: &Path, to: &Path, rel: &str) -> Copied {
    if let Some(parent) = to.parent() {
        let made = std::fs::create_dir_all(parent);
        made.map_err(|err| io_error("create", parent, &err))?;
    }
    let bytes = std::fs::read(from);
    let bytes = bytes.map_err(|err| io_error("read", from, &err))?;
    std::fs::write(to, &bytes).map_err(|err| io_error("write", to, &err))?;
    let permissions = std::fs::metadata(from).map(|m| m.permissions());
    if let Ok(permissions) = permissions {
        let kept = std::fs::set_permissions(to, permissions);
        kept.map_err(|err| io_error("write", to, &err))?;
    }
    Ok(Entry {
        path: rel.to_owned(),
        bytes: bytes.len() as u64,
        sha256: sha256(&bytes),
    })
}

/// SHA-256 of `bytes` in lowercase hexadecimal.
pub(crate) fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
