//! Planned file writes: validated, previewable and never overwriting.
//!
//! A [`Plan`] collects files relative to a root. [`Plan::check`] decides,
//! without writing anything, what each would do: create a new file, leave
//! an identical one alone, or conflict with user content. Paths that are
//! absolute, climb with `..`, pass through a symbolic link or collide
//! inside the plan are rejected.
//! [`Plan::apply`] writes each file atomically and refuses conflicts.

use super::diff::unified;
use crate::error::CliError;
use crate::fsio::{io_error, write_atomic};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// What applying a planned file does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// The file does not exist and will be created.
    Create,
    /// The file already has these contents.
    Unchanged,
    /// User content would be overwritten; nothing is written.
    Conflict {
        /// Unified diff from the existing file to the planned one.
        diff: String,
    },
}

/// One checked file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Change {
    /// Path relative to the plan root, with `/` separators.
    pub path: String,
    /// What happens to it.
    pub action: Action,
    contents: String,
}

type Done = Result<(), CliError>;

/// Files to write under one root.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    root: PathBuf,
    files: BTreeMap<PathBuf, String>,
}

impl Plan {
    /// An empty plan for `root`.
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            files: BTreeMap::new(),
        }
    }

    /// Plan `contents` at `rel`.
    ///
    /// Errors:
    ///   A usage error for an absolute path, `..`, an empty path, or a
    ///   path planned twice or both as a file and as a directory.
    pub(crate) fn add(&mut self, rel: &str, text: impl Into<String>) -> Done {
        let path = relative(rel)?;
        let clash = self
            .files
            .keys()
            .find(|o| o.starts_with(&path) || path.starts_with(o));
        if let Some(other) = clash {
            let msg = format!("`{rel}` collides with `{}`", show(other));
            return Err(CliError::usage(msg));
        }
        self.files.insert(path, text.into());
        Ok(())
    }

    /// Decide what each file would do, writing nothing.
    ///
    /// Errors:
    ///   A usage error when a path leaves the root through a symbolic
    ///   link or an existing directory sits where a file is planned, or
    ///   an IO error reading an existing file.
    pub(crate) fn check(&self) -> Result<Vec<Change>, CliError> {
        let mut changes = Vec::with_capacity(self.files.len());
        for (rel, contents) in &self.files {
            let target = self.root.join(rel);
            no_links(&self.root, rel)?;
            let path = show(rel);
            let action = if target.is_dir() {
                let msg = format!("`{path}` is a directory");
                return Err(CliError::usage(msg));
            } else if target.exists() {
                let old = std::fs::read_to_string(&target)
                    .map_err(|err| io_error("read", &target, &err))?;
                let diff = unified(&path, &old, contents);
                if diff.is_empty() {
                    Action::Unchanged
                } else {
                    Action::Conflict { diff }
                }
            } else {
                Action::Create
            };
            changes.push(Change {
                path,
                action,
                contents: contents.clone(),
            });
        }
        Ok(changes)
    }

    /// Write every created or replaced file.
    ///
    /// Errors:
    ///   A usage error listing conflicts (nothing is written then), or an
    ///   IO error; files are written one by one, each atomically.
    pub(crate) fn apply(&self) -> Result<Vec<Change>, CliError> {
        let changes = self.check()?;
        let conflicts: Vec<&str> = changes
            .iter()
            .filter(|c| matches!(c.action, Action::Conflict { .. }))
            .map(|c| c.path.as_str())
            .collect();
        if !conflicts.is_empty() {
            let list = conflicts.join(", ");
            let msg = format!(
                "not overwriting existing files with other contents: {list} \
                 (see --dry-run)"
            );
            return Err(CliError::usage(msg));
        }
        for change in &changes {
            let target = self.root.join(&change.path);
            if change.action != Action::Create {
                continue;
            }
            if let Some(dir) = target.parent() {
                let created = std::fs::create_dir_all(dir);
                created.map_err(|err| io_error("create", dir, &err))?;
            }
            write_atomic(&target, change.contents.as_bytes(), false)?;
        }
        Ok(changes)
    }
}

/// The exact preview of `changes`: full contents of new files, diffs of
/// replaced and conflicting ones.
pub(crate) fn preview(changes: &[Change]) -> String {
    let mut out = String::new();
    for change in changes {
        match &change.action {
            Action::Create => {
                out.push_str(&format!("create {}\n", change.path));
                let diff = unified(&change.path, "", &change.contents);
                out.push_str(&diff);
            }
            Action::Unchanged => {
                out.push_str(&format!("unchanged {}\n", change.path));
            }
            Action::Conflict { diff } => {
                let path = &change.path;
                out.push_str(&format!("conflict {path} (kept)\n{diff}"));
            }
        }
    }
    out
}

/// `rel` as a plain relative path of normal components.
fn relative(rel: &str) -> Result<PathBuf, CliError> {
    let path = Path::new(rel);
    let plain = path.components().all(|c| matches!(c, Component::Normal(_)));
    if rel.is_empty() || !plain {
        let msg = format!("`{rel}` must be a relative path without `..`");
        return Err(CliError::usage(msg));
    }
    Ok(path.to_path_buf())
}

/// Reject a path whose existing part passes through a symbolic link, so
/// a write cannot land outside `root`.
fn no_links(root: &Path, rel: &Path) -> Result<(), CliError> {
    let mut current = root.to_path_buf();
    for part in rel.components() {
        current.push(part);
        match current.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() => {
                let msg = format!(
                    "`{}` is a symbolic link; refusing to write through it",
                    show(rel)
                );
                return Err(CliError::usage(msg));
            }
            Ok(_) => {}
            Err(_) => return Ok(()),
        }
    }
    Ok(())
}

/// A relative path with `/` separators on every platform.
fn show(path: &Path) -> String {
    let parts: Vec<_> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect();
    parts.join("/")
}
