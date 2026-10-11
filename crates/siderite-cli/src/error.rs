//! Errors of the application command line.
//!
//! No variant carries a database URL, password or other secret: URLs are
//! reduced to their scheme before they reach an error message.

use siderite_core::ServerError;
use siderite_migrations::MigrationError;
use thiserror::Error;

/// Failure of an [`AppCli`](crate::AppCli) command.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CliError {
    /// Bad command line (unknown command or flag, missing value).
    #[error("{0}")]
    Usage(String),
    /// No database URL is configured for `alias`.
    #[error(
        "no database configured for alias `{alias}` (set it in the settings, or pass --database-url)"
    )]
    NoDatabase {
        /// The alias that was requested.
        alias: String,
    },
    /// Opening the database `alias` failed.
    #[error("cannot connect to database `{alias}`: {source}")]
    Connect {
        /// The alias that failed.
        alias: String,
        /// The connection failure.
        #[source]
        source: MigrationError,
    },
    /// A migration command failed.
    #[error(transparent)]
    Migration(#[from] MigrationError),
    /// The server failed to start or stopped with an error.
    #[error(transparent)]
    Server(#[from] ServerError),
    /// The OpenAPI document could not be generated.
    #[error("cannot generate the OpenAPI document: {0}")]
    OpenApi(String),
    /// A native database client could not be started.
    #[error("cannot run `{program}`: {reason}")]
    Shell {
        /// Client program name (`psql`, `mysql`, `sqlite3`).
        program: &'static str,
        /// Why it could not run.
        reason: String,
    },
    /// Creating a project or invoking cargo failed.
    #[error("{0}")]
    Io(String),
    /// A file would overwrite user content; nothing was written.
    #[error("{0}")]
    Conflict(String),
    /// A live operation did not finish within its bound.
    #[error("timed out after {after:?} {what}")]
    Timeout {
        /// What was being done.
        what: String,
        /// The bound that expired.
        after: std::time::Duration,
    },
}

impl CliError {
    /// Usage error helper.
    pub fn usage(message: impl Into<String>) -> Self {
        Self::Usage(message.into())
    }

    /// Process exit status for this error: `2` for usage errors, else `1`.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) | Self::Migration(MigrationError::Usage(_)) => 2,
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_errors_exit_with_two() {
        assert_eq!(CliError::usage("bad").exit_code(), 2);
        assert_eq!(CliError::from(MigrationError::usage("bad")).exit_code(), 2);
        assert_eq!(
            CliError::NoDatabase {
                alias: "default".into()
            }
            .exit_code(),
            1
        );
    }
}
