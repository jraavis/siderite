//! A step's output: cargo's JSON messages become structured compiler
//! diagnostics; everything else is plain text for the output tail.

use crate::cargo_msg::{self, CargoMessage, CompilerDiagnostic};
use crate::report::redact_text;
use std::collections::HashSet;

/// How many compiler diagnostics a step keeps.
pub(crate) const MAX_DIAGNOSTICS: usize = 50;

/// What a step printed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Captured {
    /// Distinct compiler diagnostics, errors first, at most
    /// [`MAX_DIAGNOSTICS`], redacted.
    pub diagnostics: Vec<CompilerDiagnostic>,
    /// Distinct diagnostics left out by the bound.
    pub omitted: usize,
    /// stderr, then the stdout lines that are not cargo messages.
    pub text: String,
}

/// Split `stdout` (JSON messages when `json`) and `stderr`.
pub(crate) fn capture(stdout: &str, stderr: &str, json: bool) -> Captured {
    let mut text = stderr.to_owned();
    if !json {
        text.push_str(stdout);
        return Captured {
            text,
            ..Captured::default()
        };
    }
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for line in stdout.lines() {
        match cargo_msg::parse(line) {
            // Cargo replays cached messages; each counts once.
            CargoMessage::Compiler(diag) if seen.insert(diag.clone()) => {
                found.push(diag);
            }
            CargoMessage::Text => {
                text.push_str(line);
                text.push('\n');
            }
            _ => {}
        }
    }
    // Errors first; the order within a level is rustc's.
    found.sort_by_key(|d| d.level != "error");
    let omitted = found.len().saturating_sub(MAX_DIAGNOSTICS);
    found.truncate(MAX_DIAGNOSTICS);
    let diagnostics = found.into_iter().map(redacted).collect();
    Captured {
        diagnostics,
        omitted,
        text,
    }
}

fn redacted(mut diag: CompilerDiagnostic) -> CompilerDiagnostic {
    diag.message = redact_text(&diag.message);
    diag.rendered = diag.rendered.as_deref().map(redact_text);
    for span in &mut diag.spans {
        span.label = span.label.as_deref().map(redact_text);
    }
    diag
}
