//! `siderite-backends`: query compilers and database adapters.
//!
//! * [`sql`] compiles a [`QueryPlan`](siderite_orm::QueryPlan) into
//!   parameterised SQL for a [`Dialect`](sql::Dialect). Values are **always**
//!   bound, never interpolated.
//! * [`sqlite`] (feature `sqlite`, default) executes plans with SQLx;
//!   `sqlite::native` (feature `sqlite-native`) runs them on SQLite in
//!   process, without SQLx's worker thread.
//! * [`postgres`] (feature `postgres`) executes plans with SQLx.
//!
//! * [`mysql`] (feature `mysql`) executes plans with SQLx.
//! * [`mongodb`] (feature `mongodb`) compiles the supported plan subset to
//!   filters and aggregation pipelines.
//! * [`redis`] (feature `redis`) is a key/hash/set store
//!   ([`redis::RedisStore`]), not a QuerySet backend.
#![forbid(unsafe_code)]

#[cfg(any(feature = "postgres", feature = "mysql"))]
pub mod connection_init;
#[cfg(feature = "mongodb")]
pub mod mongodb;
#[cfg(feature = "mysql")]
pub mod mysql;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "redis")]
pub mod redis;
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
mod shared;
pub mod sql;
#[cfg(feature = "sqlite")]
pub mod sqlite;
