//! `siderite` CLI: `new`, `run`, migrations, and other project commands.

#![forbid(unsafe_code)]

use std::process::ExitCode as ProcessExit;

#[tokio::main]
async fn main() -> ProcessExit {
    match siderite_cli::run().await {
        Ok(code) => ProcessExit::from(code),
        // `run` has already reported the error (as text or a JSON envelope).
        Err(err) => ProcessExit::from(err.exit_code()),
    }
}
