//! Driver errors classified like [`shared::map_error`](crate::shared::map_error)
//! classifies SQLx's SQLite errors, with the same messages.
//!
//! SQLx reports a constraint violation for the extended result codes of
//! unique and primary-key, foreign-key and check constraints; every other
//! failure, `NOT NULL` violations included, is a database error formatted as
//! `error returned from database: (code: N) message`.

use rusqlite::ffi;
use siderite_orm::BackendError;

/// Classify a rusqlite error.
pub(super) fn map(failure: rusqlite::Error) -> BackendError {
    match &failure {
        rusqlite::Error::SqliteFailure(code, message) => {
            let text = message.clone().unwrap_or_else(|| code.to_string());
            classify(code.extended_code, text)
        }
        rusqlite::Error::SqlInputError { error, msg, .. } => {
            classify(error.extended_code, msg.clone())
        }
        _ => BackendError::Database(failure.to_string()),
    }
}

fn classify(code: i32, message: String) -> BackendError {
    match code {
        ffi::SQLITE_CONSTRAINT_UNIQUE
        | ffi::SQLITE_CONSTRAINT_PRIMARYKEY
        | ffi::SQLITE_CONSTRAINT_FOREIGNKEY
        | ffi::SQLITE_CONSTRAINT_CHECK => BackendError::Constraint(message),
        _ => BackendError::Database(format!(
            "error returned from database: (code: {code}) {message}"
        )),
    }
}
