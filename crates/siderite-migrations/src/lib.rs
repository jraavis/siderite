//! Django-inspired schema migrations for siderite.
//!
//! * [`state`] — serializable snapshots built from [`siderite_orm::ModelMeta`].
//! * [`operation`] — `CreateModel`, `AddField`, `RunSQL`, …
//! * [`autodetector`] — deterministic [`autodetector::diff`].
//! * [`schema_editor`] — PostgreSQL / SQLite / MySQL DDL.
//! * [`loader`] / [`migration`] — JSON files, dependency graph, checksums.
//! * [`executor::Migrator`] — history table, migrate / rollback / dry-run.
//! * [`cli::run`] — the five management commands, called from the app binary.
//!
//! See `docs/MIGRATIONS.md` for the file format, reversibility table and
//! SQLite rebuild caveat.

#![forbid(unsafe_code)]

pub mod autodetector;
pub mod cli;
pub mod error;
pub mod executor;
mod hash;
pub mod loader;
pub mod migration;
pub mod operation;
pub mod registry;
pub mod schema_editor;
pub mod squash;
pub mod state;

pub use autodetector::{RenameHints, diff, diff_with};
pub use cli::{ExitCode, make_migrations, run};
pub use error::MigrationError;
pub use executor::LockHolder;
pub use executor::{
    HISTORY_TABLE, MigrationIntent, Migrator, RecoveryEntry, RecoveryReport, Report,
};
pub use loader::{MigrationGraph, load_dir, write_migration};
pub use migration::Migration;
pub use operation::Operation;
pub use registry::MigrationRegistry;
pub use state::{
    ConstraintState, DbDefault, FieldState, ForeignKeyState, IndexState, ModelState, OnDelete,
    ProjectState, SqlType,
};
