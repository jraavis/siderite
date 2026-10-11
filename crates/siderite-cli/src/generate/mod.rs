//! Shared helpers for commands that write source files (`new`, `ai init`
//! and the generators): validated identifiers, a write plan that never
//! overwrites user files, exact dry-run previews and line diffs.

pub(crate) mod command;
pub(crate) mod diff;
pub(crate) mod ident;
pub(crate) mod model;
pub(crate) mod plan;
pub(crate) mod resource;
pub(crate) mod route;

#[cfg(test)]
mod tests;

pub(crate) use plan::{Plan, preview, summary};

use std::io::Write;
use std::process::{Command, Stdio};

/// `source` as `rustfmt` lays it out, or unchanged when rustfmt is not
/// installed or rejects it, so generated files match `cargo fmt`.
pub(crate) fn rustfmt(source: &str) -> String {
    let child = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return source.to_owned();
    };
    if let Some(mut stdin) = child.stdin.take() {
        // Dropping stdin closes it, so rustfmt sees the end of input.
        let _ = stdin.write_all(source.as_bytes());
    }
    match child.wait_with_output() {
        Ok(out) if out.status.success() && !out.stdout.is_empty() => {
            String::from_utf8(out.stdout).unwrap_or_else(|_| source.into())
        }
        _ => source.to_owned(),
    }
}
