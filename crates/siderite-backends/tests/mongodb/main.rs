//! MongoDB end-to-end tests.
//!
//! These tests are explicitly ignored in offline runs. Select --ignored
//! with the documented service URL to run them; missing configuration fails.
//! Each test owns a disposable schema/database or a unique Redis namespace.
//! Shared application data and Redis FLUSH commands are not used.
#![cfg(feature = "mongodb")]
#![allow(clippy::unwrap_used)]

#[path = "../common/mod.rs"]
mod common;
mod fixtures;
mod functions;
mod lookups;
mod queries;
mod storage;
mod transactions;
mod unsupported;
mod writes;
