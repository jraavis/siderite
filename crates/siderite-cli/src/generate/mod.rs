//! Shared helpers for commands that write source files (`new`, `ai init`
//! and the generators): validated identifiers, a write plan that never
//! overwrites user files, exact dry-run previews and line diffs.

pub(crate) mod diff;
pub(crate) mod ident;
pub(crate) mod plan;

#[cfg(test)]
mod tests;

pub(crate) use plan::{Plan, preview};
