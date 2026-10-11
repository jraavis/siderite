//! Shared fixtures for the roundtrip tests.

use siderite_backends::sqlite::SqliteBackend;
use siderite_orm::Db;

/// Open a fresh in-memory SQLite database.
pub(crate) async fn memory_db() -> Db {
    Db::new(SqliteBackend::connect("sqlite::memory:").await.unwrap())
}
