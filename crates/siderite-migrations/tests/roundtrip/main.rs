//! Full SQLite round trip: makemigrations → migrate → DML → add field →
//! rollback → showmigrations, plus dry-run, checksum mismatch, irreversible.
#![allow(clippy::unwrap_used)]

#[path = "../common/mod.rs"]
mod common;
mod fixtures;

mod basic;
mod fk_checks;
mod not_null;
mod rebuild_data;
mod runs;
mod triggers;
