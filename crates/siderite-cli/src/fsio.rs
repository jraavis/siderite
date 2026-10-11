//! File writes shared by commands that produce files.
//!
//! Writes go to a temporary sibling first, are flushed to disk, and only
//! then appear under their final name, so a reader never sees a partial
//! file and a failed write leaves the old one intact.

use crate::error::CliError;
use std::fs::{File, OpenOptions};
use std::io::{Error as IoError, ErrorKind, Write};
use std::path::{Path, PathBuf};

type Done = Result<(), CliError>;

/// A name that no other call in this or another live process produces:
/// `{prefix}-{pid}-{nanos}-{counter}{suffix}`.
pub(crate) fn unique_name(prefix: &str, suffix: &str) -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("{prefix}-{pid}-{nanos}-{n}{suffix}")
}

/// Create `path`, failing if anything (a file or a link) already exists.
pub(crate) fn create_new(path: &Path) -> Result<File, CliError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| io_error("create", path, &err))
}

/// The error of an IO operation `what` on `path`.
pub(crate) fn io_error(what: &str, path: &Path, err: &IoError) -> CliError {
    CliError::Io(format!("cannot {what} {}: {err}", path.display()))
}

/// Removes the temporary file unless it was renamed into place.
struct Staged(Option<PathBuf>);

impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Write `bytes` to `dest` atomically.
///
/// The bytes are written and synced to a new temporary file in the same
/// directory, which then takes the final name in one step. Without
/// `force` an existing `dest` is never replaced: the final step is a
/// hard link, which fails when the name exists, so a file created
/// concurrently is not clobbered either. Where hard links are not
/// supported, existence is checked right before the rename instead.
///
/// Args:
///   dest: The file to write; its directory must exist.
///   bytes: The complete contents.
///   force: Whether an existing `dest` may be replaced.
///
/// Errors:
///   [`CliError::Usage`] when `dest` exists and `force` is false, or
///   [`CliError::Io`] when a filesystem operation fails.
pub(crate) fn write_atomic(dest: &Path, bytes: &[u8], force: bool) -> Done {
    let not_file = || format!("{} is not a file path", dest.display());
    let name = dest
        .file_name()
        .ok_or_else(|| CliError::usage(not_file()))?;
    let dir = dest.parent().filter(|d| !d.as_os_str().is_empty());
    let dir = dir.unwrap_or(Path::new("."));
    let prefix = format!(".{}", name.to_string_lossy());
    let tmp = dir.join(unique_name(&prefix, ".tmp"));
    let mut staged = Staged(Some(tmp.clone()));
    let mut file = create_new(&tmp)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|err| io_error("write", &tmp, &err))?;
    drop(file);
    if force {
        let renamed = std::fs::rename(&tmp, dest);
        renamed.map_err(|err| io_error("replace", dest, &err))?;
        staged.0 = None;
        return Ok(());
    }
    match std::fs::hard_link(&tmp, dest) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(exists(dest)),
        Err(_) if dest.exists() => Err(exists(dest)),
        Err(_) => {
            let renamed = std::fs::rename(&tmp, dest);
            renamed.map_err(|err| io_error("create", dest, &err))?;
            staged.0 = None;
            Ok(())
        }
    }
}

fn exists(dest: &Path) -> CliError {
    CliError::usage(format!(
        "{} already exists; pass --force to replace it",
        dest.display()
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::fixtures::ScratchDir;

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn writes_new_files_and_refuses_to_overwrite() {
        let scratch = ScratchDir::new("fsio");
        let dest = scratch.path().join("out.json");
        write_atomic(&dest, b"one", false).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"one");
        let err = write_atomic(&dest, b"two", false).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("--force"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"one");
        write_atomic(&dest, b"three", true).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"three");
        assert!(leftovers(scratch.path()).is_empty());
    }

    #[test]
    fn a_missing_directory_is_an_error_without_leftovers() {
        let scratch = ScratchDir::new("fsio-missing");
        let dest = scratch.path().join("nope/out.json");
        let err = write_atomic(&dest, b"x", false).unwrap_err();
        assert_eq!(err.exit_code(), 1);
        assert!(leftovers(scratch.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_link_at_the_destination_is_not_followed() {
        let scratch = ScratchDir::new("fsio-link");
        let target = scratch.path().join("elsewhere");
        let dest = scratch.path().join("out.json");
        std::os::unix::fs::symlink(&target, &dest).unwrap();
        assert!(write_atomic(&dest, b"x", false).is_err());
        assert!(!target.exists());
    }
}
