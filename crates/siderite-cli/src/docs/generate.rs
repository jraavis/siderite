//! Builds `index.json` from the repository's public documentation and
//! compiled examples. A test compares the packaged file with a fresh
//! build; `SIDERITE_UPDATE_DOCS_INDEX=1 cargo test -p siderite-cli docs`
//! rewrites it.

use super::index::{Entry, INDEX_FORMAT, Index, Kind};
use std::path::{Path, PathBuf};

/// Website sections that document the public API.
const SECTIONS: &[&str] = &["start", "guides", "reference", "tutorials"];
/// Website pages, relative to the repository root.
const SITE: &str = "website/src/content/docs";
/// Crates whose Cargo features the docs may name.
const FEATURE_CRATES: &[&str] = &[
    "siderite-backends",
    "siderite-cache",
    "siderite-cli",
    "siderite-testkit",
];

/// The repository root, when the sources are present (not in a packaged
/// crate).
pub(crate) fn repo_root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.join(SITE).is_dir().then_some(root)
}

/// The index for the sources under `root`.
pub(crate) fn build(root: &Path) -> Index {
    let features = known_features(root);
    let mut entries = Vec::new();
    for section in SECTIONS {
        for file in files(&root.join(SITE).join(section), "md") {
            let rel = relative(root, &file);
            let text = read(&file);
            entries.extend(page(&rel, &text, &features));
        }
    }
    let mut examples = files(&root.join("examples"), "rs");
    examples.extend(files(
        &root.join("crates/siderite-cli/src/ai/examples"),
        "rs",
    ));
    for file in examples {
        let rel = relative(root, &file);
        entries.push(example(&rel, &read(&file), &features));
    }
    Index {
        format: INDEX_FORMAT,
        siderite: env!("CARGO_PKG_VERSION").to_owned(),
        entries,
    }
}

fn read(path: &Path) -> String {
    let read = std::fs::read_to_string(path);
    read.unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn relative(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

/// Files with `extension` under `dir`, sorted, skipping build output.
fn files(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(listing) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in listing.flatten() {
            let path = item.path();
            let name = item.file_name();
            if path.is_dir() && name != "target" {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == extension) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Feature names declared in the `[features]` tables of the crates.
fn known_features(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for krate in FEATURE_CRATES {
        let dir = root.join("crates").join(krate);
        let manifest = read(&dir.join("Cargo.toml"));
        let mut inside = false;
        for line in manifest.lines() {
            if line.starts_with('[') {
                inside = line.trim() == "[features]";
            } else if inside && let Some((name, _)) = line.split_once('=') {
                let name = name.trim();
                let valid = name.chars().all(is_feature_char);
                if valid && !name.is_empty() && name != "default" {
                    names.push(name.to_owned());
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

fn is_feature_char(c: char) -> bool {
    c.is_ascii_lowercase() || c == '-'
}

/// The features `text` says are needed: `features = [.."name"..]`, "the
/// `name` feature" or "feature `name`".
fn features_in(text: &str, known: &[String]) -> Vec<String> {
    let mut found: Vec<String> = known
        .iter()
        .filter(|name| {
            let ticked = format!("`{name}`");
            let quoted = format!("\"{name}\"");
            text.contains(&format!("{ticked} feature"))
                || text.contains(&format!("feature {ticked}"))
                || text
                    .lines()
                    .any(|l| l.contains("features") && l.contains(&quoted))
        })
        .cloned()
        .collect();
    found.sort();
    found
}

/// The front-matter title of a page.
fn title(text: &str) -> Option<&str> {
    let body = text.strip_prefix("---\n")?;
    let (front, _) = body.split_once("\n---")?;
    front
        .lines()
        .find_map(|l| l.strip_prefix("title:"))
        .map(str::trim)
}

/// The anchor of a heading, as the website renders it.
fn slug(heading: &str) -> String {
    let mut out = String::new();
    for c in heading.trim().chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else if c == ' ' {
            out.push('-');
        }
    }
    out
}

/// A page split at its `## ` headings (outside code blocks).
fn page(path: &str, text: &str, known: &[String]) -> Vec<Entry> {
    let page_title = title(text).unwrap_or(path).to_owned();
    let page_id = path
        .trim_start_matches(SITE)
        .trim_start_matches('/')
        .trim_end_matches(".md")
        .to_owned();
    let mut entries = Vec::new();
    let mut heading: Option<String> = None;
    let mut start = 1u32;
    let mut body = String::new();
    let mut in_code = false;
    let mut flush = |heading: &Option<String>, start: u32, body: &str| {
        let trimmed = body.trim();
        if trimmed.is_empty() {
            return;
        }
        let (id, title) = match heading {
            Some(h) => (
                format!("{page_id}#{}", slug(h)),
                format!("{page_title} > {h}"),
            ),
            None => (page_id.clone(), page_title.clone()),
        };
        entries.push(Entry {
            id,
            kind: Kind::Guide,
            title,
            path: path.to_owned(),
            line: start,
            features: features_in(trimmed, known),
            text: trimmed.to_owned(),
        });
    };
    let content_start = front_matter_lines(text);
    for (n, line) in text.lines().enumerate().skip(content_start) {
        let number = u32::try_from(n + 1).unwrap_or(u32::MAX);
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
        }
        if !in_code && let Some(h) = line.strip_prefix("## ") {
            flush(&heading, start, &body);
            heading = Some(h.trim().to_owned());
            start = number;
            body.clear();
            continue;
        }
        if body.is_empty() && heading.is_none() {
            start = number;
        }
        body.push_str(line);
        body.push('\n');
    }
    flush(&heading, start, &body);
    entries
}

/// Lines taken by the front matter.
fn front_matter_lines(text: &str) -> usize {
    if !text.starts_with("---\n") {
        return 0;
    }
    let close = text.lines().skip(1).position(|l| l == "---");
    close.map_or(0, |i| i + 2)
}

/// An example source file as one entry.
fn example(path: &str, text: &str, known: &[String]) -> Entry {
    let doc = text
        .lines()
        .find_map(|l| l.strip_prefix("//!"))
        .map(str::trim)
        .filter(|d| !d.is_empty());
    let title = match doc {
        Some(doc) => format!("Example {path}: {doc}"),
        None => format!("Example {path}"),
    };
    Entry {
        id: path.to_owned(),
        kind: Kind::Example,
        title,
        path: path.to_owned(),
        line: 1,
        features: features_in(text, known),
        text: text.trim_end().to_owned(),
    }
}
