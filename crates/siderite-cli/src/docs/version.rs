//! Which siderite version a project actually builds with, from its
//! `Cargo.lock`, to warn when it differs from the packaged docs.

use std::path::Path;

/// The framework crate.
const FRAMEWORK: &str = "siderite";

/// The siderite versions locked in the nearest `Cargo.lock` above `dir`
/// (sorted, distinct); empty when there is no lock file or no siderite.
pub(crate) fn installed(dir: &Path) -> Vec<String> {
    let lock = dir
        .ancestors()
        .map(|d| d.join("Cargo.lock"))
        .find(|p| p.is_file());
    let text = lock.and_then(|p| std::fs::read_to_string(p).ok());
    text.map_or_else(Vec::new, |t| locked(&t, FRAMEWORK))
}

/// Versions of `name` in lock file `text`.
pub(crate) fn locked(text: &str, name: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current: Option<&str> = None;
    for line in text.lines().map(str::trim) {
        if line == "[[package]]" {
            current = None;
        } else if let Some(value) = string_value(line, "name") {
            current = Some(value);
        } else if let Some(value) = string_value(line, "version")
            && current == Some(name)
        {
            found.push(value.to_owned());
        }
    }
    found.sort();
    found.dedup();
    found
}

/// `value` of a `key = "value"` line.
fn string_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(key)?.trim_start().strip_prefix('=')?;
    rest.trim().strip_prefix('"')?.strip_suffix('"')
}
