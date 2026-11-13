//! Which SQLite backend the SQLite tests run against.
//!
//! Tests build their backend with `TestSqlite::connect(url)`. That is the
//! native backend when this crate's `sqlite-native` feature is on and
//! SQLx's otherwise, so `cargo test --features sqlite-native` runs every
//! SQLite test on the native backend. Mirrors
//! `siderite-backends/tests/common/backend.rs`; the unit tests and suites
//! without the rest of `common` include this file alone with `#[path]`.

/// The SQLite backend under test: `NativeSqliteBackend`.
#[cfg(feature = "sqlite-native")]
pub type TestSqlite = siderite_backends::sqlite::native::NativeSqliteBackend;

/// The SQLite backend under test: `SqliteBackend` (SQLx).
#[cfg(not(feature = "sqlite-native"))]
pub type TestSqlite = siderite_backends::sqlite::SqliteBackend;
