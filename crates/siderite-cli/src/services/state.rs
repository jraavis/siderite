//! A project's service state under `.siderite/local/`: names, the local
//! port and the generated credentials. The directory ignores itself in
//! git and, on Unix, only its owner can read it.

use crate::error::CliError;
use crate::fsio::{io_error, write_atomic};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The state directory, relative to the package directory.
pub(crate) const DIR: &str = ".siderite/local";

/// The recorded state of the PostgreSQL service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Postgres {
    /// Random project id in resource names.
    pub id: String,
    /// The database user.
    pub user: String,
    /// The generated password.
    pub password: String,
    /// The database name.
    pub database: String,
    /// The host port, bound to 127.0.0.1 only.
    pub port: u16,
    /// The image.
    pub image: String,
}

fn file(dir: &Path) -> PathBuf {
    dir.join(DIR).join("postgres.json")
}

/// The recorded state, if any.
///
/// Errors:
///   An IO error, or a usage error for a file that does not parse.
pub(crate) fn load(dir: &Path) -> Result<Option<Postgres>, CliError> {
    let path = file(dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_error("read", &path, &err)),
    };
    let parsed = serde_json::from_str(&text).map_err(|err| {
        let shown = path.display();
        CliError::usage(format!("{shown} is not valid service state: {err}"))
    })?;
    Ok(Some(parsed))
}

/// Record `state`, creating the private, git-ignored directory.
///
/// Errors:
///   An IO error.
pub(crate) fn save(dir: &Path, state: &Postgres) -> Result<(), CliError> {
    let local = dir.join(DIR);
    create_private(&local)?;
    let ignore = local.join(".gitignore");
    if !ignore.exists() {
        write_atomic(&ignore, b"*\n", true)?;
    }
    let json = serde_json::to_vec_pretty(state);
    let json = json.map_err(|err| CliError::Io(err.to_string()))?;
    write_atomic(&file(dir), &json, true)
}

/// Forget the recorded state.
///
/// Errors:
///   An IO error other than a missing file.
pub(crate) fn remove(dir: &Path) -> Result<(), CliError> {
    let path = file(dir);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io_error("remove", &path, &err)),
    }
}

#[cfg(unix)]
fn create_private(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path)
        .map_err(|err| io_error("create", path, &err))?;
    let private = std::fs::Permissions::from_mode(0o700);
    let protected = std::fs::set_permissions(path, private);
    protected.map_err(|err| io_error("protect", path, &err))
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> Result<(), CliError> {
    std::fs::create_dir_all(path).map_err(|err| io_error("create", path, &err))
}
