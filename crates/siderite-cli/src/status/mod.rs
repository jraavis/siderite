//! `showmigrations --json`: live migration status as a typed report.
//!
//! The rows come from `Migrator::show`, the same call behind the text
//! output, never from parsing printed lines. The connection and the
//! history read together are bounded by a timeout, and errors name the
//! alias, never the URL.

use crate::connect::connect_url;
use crate::error::CliError;
use crate::report::{Envelope, MigrationStatusData, UrlSource, build};
use siderite_migrations::{MigrationGraph, Migrator, load_dir};
use std::path::Path;
use std::time::Duration;

/// Bound on connecting and reading the history for a status report.
pub(crate) const STATUS_TIMEOUT: Duration = Duration::from_secs(30);

/// The database a live command uses, and how it was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selection {
    /// Connection URL; never printed.
    pub url: String,
    /// The alias, `None` for the standalone binary.
    pub alias: Option<String>,
    /// Where `url` came from.
    pub source: UrlSource,
}

/// Build the `showmigrations` report for `selection` and the migrations
/// in `dir`, within `timeout`.
///
/// Errors:
///   A migration error for unreadable files or history, a connection
///   error naming the alias, or [`CliError::Timeout`]. A failure is never
///   reported as an empty, successful status.
pub(crate) async fn status(
    selection: &Selection,
    dir: &Path,
    timeout: Duration,
) -> Result<Envelope<MigrationStatusData>, CliError> {
    let graph = MigrationGraph::build(load_dir(dir)?)?;
    let alias = selection.alias.as_deref();
    let read = async {
        let db = connect_url(&selection.url)
            .await
            .map_err(|source| match alias {
                Some(alias) => CliError::Connect {
                    alias: alias.to_owned(),
                    source,
                },
                None => CliError::Migration(source),
            })?;
        Ok::<_, CliError>(Migrator::new(&db, &graph).show().await?)
    };
    let rows = tokio::time::timeout(timeout, read)
        .await
        .map_err(|_| CliError::Timeout {
            what: "reading the migration status".to_owned(),
            after: timeout,
        })??;
    Ok(build::migration_status(alias, selection.source, rows))
}

#[cfg(test)]
mod tests;
