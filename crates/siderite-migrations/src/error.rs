//! Errors raised by the migration loader, autodetector, executor and CLI.

use siderite_orm::{BackendCapabilityError, BackendKind, OrmError};
use thiserror::Error;

/// Failure of a migration command, graph check, or DDL step.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MigrationError {
    /// An explicit SQL script may have committed unrecorded effects.
    #[error(
        "migration `{id}` has uncertain RunSQL at operation {operation}; \
        reconcile its effects before clearing the intent and retrying"
    )]
    UncertainSqlStep {
        /// Migration id.
        id: String,
        /// Zero-based operation index.
        operation: usize,
    },
    /// A non-transactional callback may have committed unrecorded effects.
    #[error(
        "migration `{id}` has uncertain RunRust `{callback}` at operation \
        {operation}; reconcile its effects before clearing the intent, or \
        explicitly register it as replay safe"
    )]
    UncertainRustStep {
        /// Migration id.
        id: String,
        /// Callback name.
        callback: String,
        /// Zero-based operation index in the recorded direction.
        operation: usize,
    },
    /// The advisory migration lock could not be acquired before its deadline.
    ///
    /// Use [`Migrator::lock_holder`](crate::Migrator::lock_holder) on this
    /// path to name the session that holds it.
    #[error("timed out waiting for the migration lock")]
    LockTimeout,
    /// The run finished, but the release of the migration lock could not be
    /// confirmed (for example the lock session was lost), so lock ownership
    /// is uncertain. The listed migrations were applied (`migrate`) or
    /// unapplied (`rollback`); inspect before retrying.
    #[error(
        "the run finished for {applied:?}, but the migration lock release \
        could not be confirmed, so lock ownership is uncertain: {source}"
    )]
    LockReleaseUncertain {
        /// Ids written to (`migrate`) or removed from (`rollback`) the
        /// history table by this run.
        applied: Vec<String>,
        /// Why the release could not be confirmed.
        #[source]
        source: Box<MigrationError>,
    },
    /// A migration lists a dependency that is not on disk.
    #[error("unknown dependency `{dependency}` of migration `{id}`")]
    MissingDependency {
        /// Migration that named the missing dependency.
        id: String,
        /// Missing migration id.
        dependency: String,
    },
    /// The dependency graph contains a cycle.
    #[error("cycle in migration graph: {}", .nodes.join(" -> "))]
    Cycle {
        /// Nodes of one cycle, starting and ending at the same id.
        nodes: Vec<String>,
    },
    /// More than one leaf (unmerged heads).
    #[error(
        "multiple migration heads: {}. Create a merge migration or run squashmigrations",
        .heads.join(", ")
    )]
    MultipleHeads {
        /// Leaf migration ids, sorted.
        heads: Vec<String>,
    },
    /// An applied migration's file no longer matches the checksum recorded in
    /// the history table.
    #[error("checksum mismatch for applied migration `{id}` (history {history}, file {file})")]
    ChecksumMismatch {
        /// Migration id.
        id: String,
        /// Checksum stored in `siderite_migrations`.
        history: String,
        /// Checksum of the file currently on disk.
        file: String,
    },
    /// A rollback was requested for a migration that cannot be reversed.
    #[error("cannot reverse migration `{id}`: {reason}")]
    Irreversible {
        /// Migration id.
        id: String,
        /// Why reversal is impossible.
        reason: String,
    },
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Migration JSON could not be encoded or decoded.
    #[error("invalid migration JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The ORM backend rejected a statement or connection.
    #[error(transparent)]
    Orm(#[from] OrmError),
    /// A requested SQL feature is not supported by this backend.
    #[error(transparent)]
    Capability(#[from] BackendCapabilityError),
    /// Schema migrations are only implemented for PostgreSQL, SQLite and MySQL.
    #[error("schema migrations are not supported by {0:?}")]
    UnsupportedBackend(BackendKind),
    /// The operation does not apply to the current [`crate::state::ProjectState`].
    #[error("{0}")]
    State(String),
    /// A named `RunRust` operation is not in the [`crate::registry::MigrationRegistry`].
    #[error("unregistered RunRust operation `{0}`")]
    UnregisteredRust(String),
    /// Command-line usage error.
    #[error("{0}")]
    Usage(String),
    /// MySQL DDL committed earlier statements, then a later one failed.
    #[error(
        "MySQL migration `{id}` failed on statement {index} of {total} after earlier DDL committed (repair the schema by hand, then re-run): {source}"
    )]
    MysqlPartial {
        /// Migration id.
        id: String,
        /// 1-based statement that failed.
        index: usize,
        /// Number of SQL statements in the migration.
        total: usize,
        /// Driver error from the failing statement.
        #[source]
        source: OrmError,
    },
    /// A MySQL data operation failed after earlier statements committed.
    #[error(
        "MySQL migration `{id}` failed in operation {index} of {total} ({summary}) after earlier statements committed (repair the schema by hand, then re-run): {source}"
    )]
    MysqlOpPartial {
        /// Migration id.
        id: String,
        /// 1-based operation that failed.
        index: usize,
        /// Number of operations in the migration.
        total: usize,
        /// Short label of the failing operation.
        summary: String,
        /// The operation's own error.
        #[source]
        source: Box<MigrationError>,
    },
    /// A non-transactional PostgreSQL migration (`atomic: false`) failed on a
    /// statement after earlier statements committed.
    #[error(
        "PostgreSQL migration `{id}` (atomic: false) failed on statement {index} of {total} after earlier statements committed; they ran outside a transaction and cannot be rolled back (repair the schema by hand, then re-run `migrate` to resume at the failed statement): {source}"
    )]
    PostgresPartial {
        /// Migration id.
        id: String,
        /// 1-based statement that failed.
        index: usize,
        /// Number of SQL statements in the migration.
        total: usize,
        /// Driver error from the failing statement.
        #[source]
        source: OrmError,
    },
    /// A `RunRust` step of a non-transactional PostgreSQL migration
    /// (`atomic: false`) failed after earlier statements committed.
    #[error(
        "PostgreSQL migration `{id}` (atomic: false) failed in operation {index} of {total} ({summary}) after earlier statements committed; they ran outside a transaction and cannot be rolled back (repair the schema by hand, then re-run `migrate` to resume at the failed operation): {source}"
    )]
    PostgresOpPartial {
        /// Migration id.
        id: String,
        /// 1-based operation that failed.
        index: usize,
        /// Number of operations in the migration.
        total: usize,
        /// Short label of the failing operation.
        summary: String,
        /// The operation's own error.
        #[source]
        source: Box<MigrationError>,
    },
}

impl MigrationError {
    /// [`State`](Self::State) helper.
    pub fn state(msg: impl Into<String>) -> Self {
        Self::State(msg.into())
    }

    /// [`Usage`](Self::Usage) helper.
    pub fn usage(msg: impl Into<String>) -> Self {
        Self::Usage(msg.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use siderite_orm::BackendError;

    #[test]
    fn mysql_partial_names_the_failed_statement() {
        let err = MigrationError::MysqlPartial {
            id: "0002_add".into(),
            index: 2,
            total: 3,
            source: BackendError::Database("table exists".into()).into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("statement 2 of 3"), "{msg}");
        assert!(msg.contains("0002_add"), "{msg}");
        assert!(msg.contains("repair the schema by hand"), "{msg}");
    }

    #[test]
    fn mysql_op_partial_names_the_failed_operation() {
        let err = MigrationError::MysqlOpPartial {
            id: "0003_data".into(),
            index: 2,
            total: 2,
            summary: "RunRust seed".into(),
            source: Box::new(MigrationError::UnregisteredRust("seed".into())),
        };
        let msg = err.to_string();
        assert!(msg.contains("operation 2 of 2 (RunRust seed)"), "{msg}");
        assert!(msg.contains("0003_data"), "{msg}");
        assert!(msg.contains("repair the schema by hand"), "{msg}");
    }

    #[test]
    fn postgres_partial_names_the_failed_statement() {
        let err = MigrationError::PostgresPartial {
            id: "0004_add".into(),
            index: 2,
            total: 3,
            source: BackendError::Database("relation already exists".into()).into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("statement 2 of 3"), "{msg}");
        assert!(msg.contains("0004_add"), "{msg}");
        assert!(msg.contains("atomic: false"), "{msg}");
        assert!(msg.contains("cannot be rolled back"), "{msg}");
        assert!(msg.contains("repair the schema by hand"), "{msg}");
    }

    #[test]
    fn postgres_op_partial_names_the_failed_operation() {
        let err = MigrationError::PostgresOpPartial {
            id: "0005_data".into(),
            index: 2,
            total: 2,
            summary: "RunRust seed".into(),
            source: Box::new(MigrationError::UnregisteredRust("seed".into())),
        };
        let msg = err.to_string();
        assert!(msg.contains("operation 2 of 2 (RunRust seed)"), "{msg}");
        assert!(msg.contains("0005_data"), "{msg}");
        assert!(msg.contains("atomic: false"), "{msg}");
        assert!(msg.contains("repair the schema by hand"), "{msg}");
    }
}
