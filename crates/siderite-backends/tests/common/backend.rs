//! Which SQLite backend the shared suites run against.
//!
//! The suites build their backend with `TestSqlite::connect(url)`. That is
//! the native backend when the `sqlite-native` feature is on and SQLx's
//! otherwise, so `cargo test --features sqlite-native` runs every suite on
//! the native backend. Suites without the rest of `common` include this
//! file alone with `#[path = "common/backend.rs"] mod backend;`.

/// The SQLite backend under test: `NativeSqliteBackend`.
#[cfg(feature = "sqlite-native")]
pub type TestSqlite = siderite_backends::sqlite::native::NativeSqliteBackend;

/// The SQLite backend under test: `SqliteBackend` (SQLx).
#[cfg(not(feature = "sqlite-native"))]
pub type TestSqlite = siderite_backends::sqlite::SqliteBackend;
