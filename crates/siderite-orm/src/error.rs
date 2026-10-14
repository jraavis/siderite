//! ORM error hierarchy. HTTP mapping lives in `siderite-core`.

use crate::capabilities::{BackendKind, Feature};
use crate::signals::SignalError;
use thiserror::Error;

/// A feature was requested that the backend does not support.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum BackendCapabilityError {
    /// `select_for_update` on a backend without row locking.
    #[error("row locking is not supported by {backend:?}")]
    RowLockingUnsupported {
        /// Backend that rejected the request.
        backend: BackendKind,
    },
    /// Any other unsupported feature.
    #[error("{feature:?} is not supported by {backend:?}")]
    Unsupported {
        /// Backend that rejected the request.
        backend: BackendKind,
        /// The unsupported feature.
        feature: Feature,
    },
}

impl BackendCapabilityError {
    /// Build the most specific error variant for `feature`.
    pub fn from_feature(backend: BackendKind, feature: Feature) -> Self {
        match feature {
            Feature::RowLocking => Self::RowLockingUnsupported { backend },
            feature => Self::Unsupported { backend, feature },
        }
    }
}

/// The query plan is invalid independent of any backend.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum QueryError {
    /// `get()` matched no rows.
    #[error("no rows matched the query")]
    DoesNotExist,
    /// `get()` matched more than one row.
    #[error("expected exactly one row, found {0}")]
    MultipleObjectsReturned(u64),
    /// Structural problem in the plan.
    #[error("invalid query plan: {0}")]
    InvalidPlan(String),
    /// A statement ran on a transaction that was already committed or rolled back.
    #[error("the transaction is already closed")]
    TransactionClosed,
    /// Cancelled or failed scope cleanup made this transaction unusable.
    #[error("the transaction was aborted; its writes cannot be committed")]
    TransactionAborted,
    /// Another statement or child scope owns this transaction connection.
    #[error("another statement or child scope owns this transaction")]
    TransactionBusy,
    /// A model or relation was used in a way its metadata does not allow.
    #[error("{0}")]
    Model(String),
    /// A decoded value had an unexpected type.
    #[error("cannot decode column `{column}`: {reason}")]
    Decode {
        /// Column name.
        column: String,
        /// Reason.
        reason: String,
    },
}

/// A pagination cursor was rejected.
///
/// Tokens come from clients, so they are validated, never trusted. Each
/// variant has a stable [`code`](Self::code) for API clients; the messages
/// say nothing about which query or column was involved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CursorError {
    /// Not base64url, over 1 KiB, truncated, or of the wrong direction.
    #[error("the cursor is malformed")]
    Malformed,
    /// The token layout version is not one this server reads.
    #[error("the cursor has an unknown version")]
    UnknownVersion,
    /// The token was issued for a different query.
    #[error("the cursor belongs to another query")]
    ForeignQuery,
    /// The values do not fit the ordering (count or types).
    #[error("the cursor does not fit the ordering")]
    Mismatch,
}

impl CursorError {
    /// The stable error code (`cursor.E001` to `cursor.E004`).
    pub const fn code(self) -> &'static str {
        match self {
            Self::Malformed => "cursor.E001",
            Self::UnknownVersion => "cursor.E002",
            Self::ForeignQuery => "cursor.E003",
            Self::Mismatch => "cursor.E004",
        }
    }
}

/// Driver / connection level failure.
///
/// Messages come from the driver and may contain SQL fragments; they are
/// logged, never returned to HTTP clients (see `ApiError: From<OrmError>`).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BackendError {
    /// Could not connect or acquire a connection.
    #[error("database connection error: {0}")]
    Connection(String),
    /// The database rejected or failed a statement.
    #[error("database error: {0}")]
    Database(String),
    /// A constraint (unique, foreign key, check) was violated.
    #[error("constraint violation: {0}")]
    Constraint(String),
}

/// Top-level ORM error.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OrmError {
    /// Query-construction or result error.
    #[error(transparent)]
    Query(#[from] QueryError),
    /// Backend failure.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// Capability mismatch.
    #[error(transparent)]
    Capability(#[from] BackendCapabilityError),
    /// A pagination cursor from the client was rejected.
    #[error(transparent)]
    Cursor(#[from] CursorError),
    /// A signal receiver failed. A failing `pre_*` receiver aborted the
    /// operation; a failing `post_*` receiver ran after the statement.
    /// Maps to HTTP 500 (`ApiError: From<OrmError>` falls through to internal).
    #[error(transparent)]
    Signal(#[from] SignalError),
    /// A database alias is not registered in [`Databases`](crate::Databases).
    ///
    /// A configuration problem (a router named an alias that was never
    /// registered, or there is no `"default"`), so it maps to HTTP 500.
    #[error("no database is registered under the alias `{0}`")]
    UnknownDatabase(String),
}
