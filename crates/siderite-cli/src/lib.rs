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

mod app_cli;
pub mod args;
pub mod check;
pub mod commands;
pub mod completions;
pub mod connect;
pub mod dbshell;
mod dispatch;
pub mod doctor;
pub mod envelope;
mod error;
#[cfg(test)]
mod fixtures;
pub mod project;
pub mod routes;
mod scaffold;
pub mod settings;
pub mod setup;
mod standalone;
pub mod toolchain;
pub mod verify;

pub use app_cli::AppCli;
pub use check::{CheckIssue, CheckLevel, CheckReport, check};
pub use commands::{
    CommandMeta, FlagMeta, LiveAccess, MutationKind, OutputMode, ProjectRequirement,
    command_catalog, find_command, global_flags, render_commands_text,
};
pub use connect::connect_url;
pub use dbshell::ShellCommand;
pub use dispatch::run;
pub use envelope::{CliDiagnostic, CliEnvelope, DiagnosticSeverity, ENVELOPE_SCHEMA_VERSION};
pub use error::CliError;
pub use project::{ResolvedProject, resolve_project};
pub use routes::{RouteRow, RoutesReport, render_routes, route_table};
pub use settings::{CliSettings, DEFAULT_ADDR};
