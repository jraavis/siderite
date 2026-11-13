//! Shared fixtures for the roundtrip tests.

use crate::common::backend::TestSqlite;
use siderite_orm::Db;

/// Open a fresh in-memory SQLite database.
pub(crate) async fn memory_db() -> Db {
    Db::new(TestSqlite::connect("sqlite::memory:").await.unwrap())
}
