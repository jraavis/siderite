//! The siderite command line.
//!
//! Two entry points:
//!
//! * [`AppCli`] is the command line of an **application binary**. It needs the
//!   application's [`App`](siderite_core::App) and model metadata, so it can
//!   serve the app (`run`), list its routes, validate it (`check`), open
//!   a database shell and run every migration command including
//!   `makemigrations`.
//! * [`run`] is the `siderite` binary. In a Cargo package it wraps `cargo run`
//!   for those commands; `new` writes a project; without a package it runs
//!   JSON-file `migrate` / `rollback` / `showmigrations` / `squashmigrations`.

#![forbid(unsafe_code)]

mod ai;
mod app_cli;
mod args;
mod cargo_msg;
pub mod check;
pub mod commands;
mod completions;
mod config;
pub mod connect;
pub mod dbshell;
mod deps;
mod dev;
mod dispatch;
mod docs;
mod doctor;
mod error;
mod explain;
#[cfg(test)]
mod fixtures;
mod fsio;
mod generate;
mod openapi;
mod project;
pub mod report;
pub mod routes;
mod scaffold;
mod services;
pub mod settings;
mod standalone;
mod status;
mod target;
mod verify;
mod workspace;

pub use app_cli::AppCli;
pub use check::{CheckIssue, CheckLevel, check};
pub use connect::connect_url;
pub use dbshell::ShellCommand;
pub use dispatch::run;
pub use error::CliError;
pub use routes::{RouteRow, render_routes, route_table};
pub use settings::{CliSettings, DEFAULT_ADDR};
