//! `ai init --link FILE`: a short managed block in an instruction file
//! of your choice (for example `AGENTS.md`), pointing at the context
//! file. Links are written only for the files named; nothing is linked
//! by default.
//!
//! The block only says where the context is; it never asks to override
//! or rank other instructions. Everything outside its two markers is
//! kept byte for byte, a file without markers gets the block appended,
//! and a rerun with nothing changed writes nothing. A file with more than
//! one block, or a broken one, is not touched: the command stops with
//! `ai.E002` and the marker lines.

use crate::error::CliError;
use std::path::{Component, Path};

const BEGIN: &str = "<!-- siderite:link begin -->";
const END: &str = "<!-- siderite:link end -->";

/// Diagnostic code of a file whose markers cannot be updated safely.
pub(crate) const CONFLICT: &str = "ai.E002";

type Merged = Result<String, CliError>;

/// The block for a link file at `file` (relative to the repository root)
/// to the context file at `context` (relative to the same root).
pub(crate) fn block(file: &str, context: &str) -> String {
    let target = relative(file, context);
    format!(
        "{BEGIN}\nThis project's siderite context (commands, conventions, \
         routes, models and checked examples) is in [{context}]({target}); \
         `siderite ai init` keeps it current.\n{END}\n"
    )
}

/// `to` as a path relative to the directory of `from`, both relative to
/// one root and `/`-separated.
fn relative(from: &str, to: &str) -> String {
    let depth = from.matches('/').count();
    format!("{}{to}", "../".repeat(depth))
}

/// A link path: relative, inside the repository, no `..`.
///
/// Errors:
///   A usage error otherwise.
pub(crate) fn checked(file: &str) -> Result<String, CliError> {
    let path = Path::new(file);
    let inside = path.components().all(|c| matches!(c, Component::Normal(_)));
    if inside && !file.is_empty() {
        return Ok(file.replace('\\', "/"));
    }
    Err(CliError::usage(format!(
        "--link `{file}` must be a path relative to the repository root, \
         without `..`"
    )))
}

/// Line numbers (1-based) of the lines containing `marker`.
fn lines(text: &str, marker: &str) -> Vec<usize> {
    let found = text.lines().enumerate().filter(|(_, l)| l.contains(marker));
    found.map(|(n, _)| n + 1).collect()
}

/// `now` (the file's text) with the block in place: replaced, or
/// appended when there is none.
///
/// Errors:
///   A conflict (`ai.E002`) for several blocks or a broken one.
pub(crate) fn merge(file: &str, now: Option<&str>, block: &str) -> Merged {
    let Some(text) = now else {
        return Ok(block.to_owned());
    };
    let (begins, ends) = (lines(text, BEGIN), lines(text, END));
    match (begins.as_slice(), ends.as_slice()) {
        ([], []) => {
            let mut out = text.to_owned();
            if !out.is_empty() {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push('\n');
            }
            out.push_str(block);
            Ok(out)
        }
        ([b], [e]) if b < e => Ok(replace(text, block)),
        _ => Err(CliError::Conflict(format!(
            "{CONFLICT}: {file} has siderite link markers that cannot be \
             updated safely (begin on lines {begins:?}, end on lines \
             {ends:?}); leave exactly one block, or remove them all"
        ))),
    }
}

/// Replace the one block in `text`, keeping every other byte.
fn replace(text: &str, block: &str) -> String {
    let start = text.find(BEGIN).unwrap_or(0);
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let end = text.find(END).map_or(text.len(), |i| i + END.len());
    let end = if text[end..].starts_with('\n') {
        end + 1
    } else {
        end
    };
    format!("{}{block}{}", &text[..line_start], &text[end..])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const CONTEXT: &str = ".siderite/context.md";

    #[test]
    fn links_are_relative_to_the_file() {
        let root = block("AGENTS.md", CONTEXT);
        assert!(root.contains("](.siderite/context.md)"), "{root}");
        let nested = block("docs/ai/notes.md", "app/.siderite/context.md");
        assert!(nested.contains("](../../app/.siderite/context.md)"));
        assert!(checked("../AGENTS.md").is_err());
        assert!(checked("/etc/AGENTS.md").is_err());
        assert_eq!(checked("docs/notes.md").unwrap(), "docs/notes.md");
    }

    #[test]
    fn text_outside_the_block_is_kept_and_reruns_are_idempotent() {
        let new = block("AGENTS.md", CONTEXT);
        assert_eq!(merge("AGENTS.md", None, &new).unwrap(), new);
        let mine = "# Rules\n\nUse tabs.";
        let once = merge("AGENTS.md", Some(mine), &new).unwrap();
        assert!(once.starts_with(mine), "{once}");
        assert_eq!(once, format!("{mine}\n\n{new}"));
        let twice = merge("AGENTS.md", Some(&once), &new).unwrap();
        assert_eq!(twice, once);
        // An older block in the middle is replaced in place.
        let old = format!("top\n{BEGIN}\nold text\n{END}\nbottom\r\n");
        let updated = merge("AGENTS.md", Some(&old), &new).unwrap();
        assert_eq!(updated, format!("top\n{new}bottom\r\n"));
    }

    #[test]
    fn broken_or_repeated_blocks_stop() {
        let new = block("AGENTS.md", CONTEXT);
        for bad in [
            format!("{BEGIN}\nno end\n"),
            format!("{END}\n{BEGIN}\n"),
            format!("{new}{new}"),
        ] {
            let err = merge("AGENTS.md", Some(&bad), &new).unwrap_err();
            assert!(err.to_string().contains(CONFLICT), "{err}");
        }
    }
}
