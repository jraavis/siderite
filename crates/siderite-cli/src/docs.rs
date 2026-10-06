//! `siderite docs search`: offline search over the framework guides.
//!
//! The index is generated from the maintained website guides
//! (`website/src/content/docs`: `start`, `guides`, `reference` and
//! `tutorials`) and checked in as `docs-index.json` next to this crate's
//! manifest, so the published CLI carries it. A unit test fails when the
//! checked-in index no longer matches the guides or the crate version;
//! regenerate it with
//! `SIDERITE_BLESS_DOCS=1 cargo test -p siderite-cli docs::tests::index_is_current`.
//!
//! Search is local keyword matching. Nothing is fetched, and no project
//! source is indexed or read except `Cargo.lock`, to compare the project's
//! `siderite` version with the index version.

use crate::args::{self, GlobalArgs};
use crate::envelope::{CliDiagnostic, CliEnvelope};
use crate::error::CliError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Format version of the search report and of the checked-in index.
pub const DOCS_FORMAT_VERSION: u32 = 1;

/// Default number of results.
pub const DEFAULT_LIMIT: usize = 5;

/// Largest accepted `--limit`.
pub const MAX_LIMIT: usize = 50;

const INDEX_JSON: &str = include_str!("../docs-index.json");

/// The checked-in documentation index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocIndex {
    /// Index format version.
    pub format_version: u32,
    /// Framework version the guides were indexed for.
    pub framework_version: String,
    /// Repository directory the guides were read from.
    pub source: String,
    /// Sections in page, then line, order.
    pub sections: Vec<DocSection>,
}

/// One heading-delimited section of a guide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocSection {
    /// Page title from the frontmatter.
    pub page: String,
    /// Section heading; the page title for text before the first heading.
    pub heading: String,
    /// Repository-relative path of the guide.
    pub path: String,
    /// 1-based line of the heading in `path`.
    pub line: usize,
    /// Path of the section on the documentation site.
    pub url: String,
    /// Cargo features the section names, e.g. `postgres`.
    pub features: Vec<String>,
    /// Section body, Markdown.
    pub text: String,
}

/// Comparison of the project's `siderite` version with the index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VersionCheck {
    /// `match`, `mismatch` or `unknown`.
    pub status: VersionStatus,
    /// `siderite` version locked by the project, when found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_version: Option<String>,
    /// Lock file the version came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_file: Option<String>,
    /// Why the status is `unknown`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Outcome of the version comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
    /// The project locks the indexed version.
    Match,
    /// The project locks a different version.
    Mismatch,
    /// No lock file, or no `siderite` package in it.
    Unknown,
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocHit {
    /// Page title.
    pub page: String,
    /// Section heading.
    pub heading: String,
    /// Repository-relative path of the guide.
    pub path: String,
    /// 1-based line of the heading.
    pub line: usize,
    /// Path of the section on the documentation site.
    pub url: String,
    /// Cargo features the section names.
    pub features: Vec<String>,
    /// First matching line of the section, shortened.
    pub snippet: String,
    /// Relevance score; higher is better.
    pub score: u32,
}

/// Result of `siderite docs search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocsSearchReport {
    /// Report format version.
    pub format_version: u32,
    /// Framework version of the searched index.
    pub framework_version: String,
    /// Repository directory the guides were indexed from.
    pub source: String,
    /// The query as searched.
    pub query: String,
    /// Project version comparison.
    pub version_check: VersionCheck,
    /// Hits, best first.
    pub results: Vec<DocHit>,
}

/// The index embedded in this binary.
///
/// # Errors
/// When the embedded JSON is malformed (a build defect).
pub fn embedded_index() -> Result<DocIndex, CliError> {
    serde_json::from_str(INDEX_JSON)
        .map_err(|err| CliError::Io(format!("embedded docs index is unreadable: {err}")))
}

fn terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for term in query
        .split(|c: char| !is_word(c))
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
    {
        if !out.contains(&term) {
            out.push(term);
        }
    }
    out
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte offsets where `needle` starts a word in `haystack` (prefix match:
/// `route` finds `routes`, `or` does not find `error`).
fn word_starts<'a>(haystack: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    haystack
        .match_indices(needle)
        .map(|(i, _)| i)
        .filter(move |&i| !haystack[..i].chars().next_back().is_some_and(is_word))
}

fn count(haystack: &str, needle: &str) -> u32 {
    u32::try_from(word_starts(haystack, needle).count()).unwrap_or(u32::MAX)
}

/// Every term must occur in the page title, heading or body. Heading hits
/// weigh most, then page title, then body occurrences (at most five per term).
fn score(section: &DocSection, terms: &[String]) -> Option<u32> {
    let heading = section.heading.to_lowercase();
    let page = section.page.to_lowercase();
    let text = section.text.to_lowercase();
    let mut total = 0u32;
    for term in terms {
        let s = count(&heading, term) * 10 + count(&page, term) * 4 + count(&text, term).min(5);
        if s == 0 {
            return None;
        }
        total += s;
    }
    Some(total)
}

/// The first line with a term, cut to at most 160 characters around the
/// first match.
fn snippet(text: &str, terms: &[String]) -> String {
    const MAX: usize = 160;
    let mut found = text.lines().map(str::trim).find_map(|l| {
        let lower = l.to_lowercase();
        terms
            .iter()
            .filter_map(|t| word_starts(&lower, t).next())
            .min()
            .map(|byte| (l, lower[..byte].chars().count()))
    });
    if found.is_none() {
        found = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| (l, 0));
    }
    let Some((line, at)) = found else {
        return String::new();
    };
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= MAX {
        return line.to_owned();
    }
    let start = at.saturating_sub(MAX / 4).min(chars.len() - MAX);
    let end = start + MAX;
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// Search `index` for `query`, best first, ties by path then line.
#[must_use]
pub fn search(index: &DocIndex, query: &str, limit: usize) -> Vec<DocHit> {
    let terms = terms(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<DocHit> = index
        .sections
        .iter()
        .filter_map(|s| {
            score(s, &terms).map(|score| DocHit {
                page: s.page.clone(),
                heading: s.heading.clone(),
                path: s.path.clone(),
                line: s.line,
                url: s.url.clone(),
                features: s.features.clone(),
                snippet: snippet(&s.text, &terms),
                score,
            })
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    hits.truncate(limit);
    hits
}

/// Every `version` of a `siderite` package in a `Cargo.lock`, in file order.
fn locked_siderite(lock: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut name: Option<&str> = None;
    let mut version: Option<&str> = None;
    let mut flush = |name: Option<&str>, version: Option<&str>| {
        if name == Some("siderite")
            && let Some(v) = version
        {
            out.push(v.to_owned());
        }
    };
    for line in lock.lines().map(str::trim) {
        if line.starts_with('[') {
            flush(name.take(), version.take());
        } else if let Some(v) = line.strip_prefix("name = ") {
            name = Some(v.trim_matches('"'));
        } else if let Some(v) = line.strip_prefix("version = ") {
            version = Some(v.trim_matches('"'));
        }
    }
    flush(name, version);
    out
}

/// Compare the `siderite` version locked at or above `start` with `indexed`.
#[must_use]
pub fn version_check(start: &Path, indexed: &str) -> VersionCheck {
    let unknown = |reason: &str, lock: Option<&Path>| VersionCheck {
        status: VersionStatus::Unknown,
        project_version: None,
        lock_file: lock.map(|p| p.display().to_string()),
        reason: Some(reason.to_owned()),
    };
    let Some(lock_path) = start
        .ancestors()
        .map(|d| d.join("Cargo.lock"))
        .find(|p| p.is_file())
    else {
        return unknown("no Cargo.lock found; version not compared", None);
    };
    let Ok(lock) = std::fs::read_to_string(&lock_path) else {
        return unknown("Cargo.lock is unreadable", Some(&lock_path));
    };
    let versions = locked_siderite(&lock);
    let Some(first) = versions.first() else {
        return unknown("Cargo.lock has no `siderite` package", Some(&lock_path));
    };
    let matched = versions.iter().any(|v| v == indexed);
    let version = if matched {
        indexed.to_owned()
    } else {
        first.clone()
    };
    VersionCheck {
        status: if matched {
            VersionStatus::Match
        } else {
            VersionStatus::Mismatch
        },
        project_version: Some(version),
        lock_file: Some(lock_path.display().to_string()),
        reason: None,
    }
}

fn mismatch_message(report: &DocsSearchReport) -> Option<String> {
    (report.version_check.status == VersionStatus::Mismatch).then(|| {
        format!(
            "these docs are for siderite {}, but the project locks siderite {}; \
             install the matching CLI or check the docs of that version",
            report.framework_version,
            report
                .version_check
                .project_version
                .as_deref()
                .unwrap_or("?")
        )
    })
}

/// Text form of `report`.
#[must_use]
pub fn render(report: &DocsSearchReport) -> String {
    let mut out = format!(
        "siderite {} docs (offline index of {})\n",
        report.framework_version, report.source
    );
    if let Some(msg) = mismatch_message(report) {
        out.push_str(&format!("warning: {msg}\n"));
    }
    if report.results.is_empty() {
        out.push_str(&format!("No results for `{}`.\n", report.query));
        return out;
    }
    for (i, hit) in report.results.iter().enumerate() {
        out.push_str(&format!(
            "\n{}. {} › {}\n   {}:{}  {}\n",
            i + 1,
            hit.page,
            hit.heading,
            hit.path,
            hit.line,
            hit.url
        ));
        if !hit.features.is_empty() {
            out.push_str(&format!("   features: {}\n", hit.features.join(", ")));
        }
        if !hit.snippet.is_empty() {
            out.push_str(&format!("   {}\n", hit.snippet));
        }
    }
    out
}

const USAGE: &str = "usage: siderite docs search QUERY... [--limit N] [--json]";

/// Parse `docs search` arguments into the query and limit.
fn parse(rest: &[String]) -> Result<(String, usize), CliError> {
    let mut words: Vec<&str> = Vec::new();
    let mut limit = DEFAULT_LIMIT;
    let mut positional: Vec<&str> = Vec::new();
    let mut iter = rest.iter().filter(|a| *a != "--json");
    while let Some(arg) = iter.next() {
        let value = if arg == "--limit" {
            Some(iter.next().ok_or_else(|| CliError::usage(USAGE))?.as_str())
        } else {
            arg.strip_prefix("--limit=")
        };
        if let Some(value) = value {
            limit = value
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=MAX_LIMIT).contains(n))
                .ok_or_else(|| {
                    CliError::usage(format!("--limit must be between 1 and {MAX_LIMIT}"))
                })?;
        } else if arg.starts_with('-') {
            // Query words are letters, digits and `_`: search `addr`, not `--addr`.
            return Err(CliError::usage(format!("unexpected flag `{arg}`; {USAGE}")));
        } else {
            positional.push(arg);
        }
    }
    let mut positional = positional.into_iter();
    if positional.next() != Some("docs") {
        return Err(CliError::usage(USAGE));
    }
    match positional.next() {
        Some("search") => {}
        Some(other) => {
            return Err(CliError::usage(format!(
                "unknown docs subcommand `{other}`; {USAGE}"
            )));
        }
        None => return Err(CliError::usage(USAGE)),
    }
    words.extend(positional);
    let query = words.join(" ");
    if terms(&query).is_empty() {
        return Err(CliError::usage(format!("missing search query; {USAGE}")));
    }
    Ok((query, limit))
}

/// Build the report for `query` against the embedded index.
///
/// # Errors
/// When the embedded index is unreadable.
pub fn search_report(
    start: &Path,
    query: &str,
    limit: usize,
) -> Result<DocsSearchReport, CliError> {
    let index = embedded_index()?;
    Ok(DocsSearchReport {
        format_version: DOCS_FORMAT_VERSION,
        version_check: version_check(start, &index.framework_version),
        results: search(&index, query, limit),
        framework_version: index.framework_version,
        source: index.source,
        query: query.to_owned(),
    })
}

/// `siderite docs search QUERY... [--limit N] [--json]`.
///
/// # Errors
/// Usage errors, or an unreadable embedded index.
pub fn run(cwd: &Path, global: &GlobalArgs, raw: &[String]) -> Result<u8, CliError> {
    let (_, rest) = args::split_global(raw)?;
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(0);
    }
    let (query, limit) = parse(&rest)?;
    let start: PathBuf = match global.manifest_path.as_deref() {
        None => cwd.to_path_buf(),
        Some(manifest) => {
            let manifest = cwd.join(manifest);
            if !manifest.is_file() {
                return Err(CliError::usage(format!(
                    "manifest path `{}` is not a file",
                    manifest.display()
                )));
            }
            manifest
                .parent()
                .map_or_else(|| cwd.to_path_buf(), Path::to_path_buf)
        }
    };
    let report = search_report(&start, &query, limit)?;
    if global.json {
        let mut env = CliEnvelope::success("docs search", report);
        if let Some(msg) = env.data.as_ref().and_then(mismatch_message) {
            env.diagnostics
                .push(CliDiagnostic::warning("DOCS_VERSION_MISMATCH", msg));
        }
        let json = env
            .to_json_pretty()
            .map_err(|err| CliError::Io(format!("failed to serialize JSON: {err}")))?;
        println!("{json}");
    } else {
        print!("{}", render(&report));
    }
    Ok(0)
}

#[cfg(test)]
mod generate {
    //! Builds [`DocIndex`] from the website guides.

    use super::{DOCS_FORMAT_VERSION, DocIndex, DocSection};
    use std::path::Path;

    /// Cargo features of the framework crates a section can name.
    const KNOWN_FEATURES: &[&str] = &[
        "postgres",
        "mysql",
        "mysql-native",
        "mongodb",
        "redis",
        "tls",
    ];

    /// Guide directories indexed, relative to the docs root.
    const DIRS: &[&str] = &["start", "guides", "reference", "tutorials"];

    pub const SOURCE: &str = "website/src/content/docs";

    fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                collect(&path, out)?;
            } else if path.extension().is_some_and(|e| e == "md" || e == "mdx") {
                out.push(path);
            }
        }
        Ok(())
    }

    /// GitHub-style heading anchor, as Starlight generates.
    pub fn slug(heading: &str) -> String {
        heading
            .to_lowercase()
            .chars()
            .filter_map(|c| match c {
                ' ' => Some('-'),
                c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
                _ => None,
            })
            .collect()
    }

    fn features(text: &str) -> Vec<String> {
        KNOWN_FEATURES
            .iter()
            .filter(|f| text.contains(&format!("`{f}`")))
            .map(|f| (*f).to_owned())
            .collect()
    }

    /// Split one guide into sections.
    pub fn sections(rel: &str, content: &str) -> Vec<DocSection> {
        let lines: Vec<&str> = content.lines().collect();
        let mut title = String::new();
        let mut start = 0;
        if lines.first() == Some(&"---")
            && let Some(end) = lines.iter().skip(1).position(|l| *l == "---")
        {
            for l in &lines[1..=end] {
                if let Some(t) = l.strip_prefix("title:") {
                    title = t.trim().trim_matches('"').trim_matches('\'').to_owned();
                }
            }
            start = end + 2;
        }
        let page_url = format!(
            "/siderite/{}/",
            rel.trim_end_matches(".mdx")
                .trim_end_matches(".md")
                .trim_end_matches("/index")
        );
        let mut out = Vec::new();
        let mut heading = title.clone();
        let mut heading_line = start + 1;
        let mut anchor: Option<String> = None;
        let mut body: Vec<&str> = Vec::new();
        let mut fence: Option<(char, usize)> = None;
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut flush = |heading: &str, line: usize, anchor: &Option<String>, body: &[&str]| {
            let text = body.join("\n").trim().to_owned();
            if text.is_empty() && anchor.is_none() {
                return;
            }
            out.push(DocSection {
                page: title.clone(),
                heading: heading.to_owned(),
                path: format!("{SOURCE}/{rel}"),
                line,
                url: match anchor {
                    Some(a) => format!("{page_url}#{a}"),
                    None => page_url.clone(),
                },
                features: features(&text),
                text,
            });
        };
        for (i, line) in lines.iter().enumerate().skip(start) {
            let trimmed = line.trim_start();
            let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~');
            let run = marker.map_or(0, |m| trimmed.chars().take_while(|c| *c == m).count());
            let was_fenced = fence.is_some();
            match (fence, marker) {
                (None, Some(m)) if run >= 3 => fence = Some((m, run)),
                (Some((m, n)), Some(c))
                    if c == m && run >= n && trimmed[run..].trim().is_empty() =>
                {
                    fence = None;
                }
                _ => {}
            }
            let h = if was_fenced || fence.is_some() {
                None
            } else {
                line.strip_prefix("## ")
                    .or_else(|| line.strip_prefix("### "))
            };
            if let Some(h) = h {
                flush(&heading, heading_line, &anchor, &body);
                body.clear();
                heading = h.trim().replace('`', "");
                heading_line = i + 1;
                let base = slug(&heading);
                let n = seen.entry(base.clone()).or_insert(0);
                anchor = Some(if *n == 0 { base } else { format!("{base}-{n}") });
                *n += 1;
            } else {
                body.push(line);
            }
        }
        flush(&heading, heading_line, &anchor, &body);
        out
    }

    /// Index every guide under `docs_root`.
    pub fn build(docs_root: &Path) -> std::io::Result<DocIndex> {
        let mut files = Vec::new();
        for dir in DIRS {
            collect(&docs_root.join(dir), &mut files)?;
        }
        let mut rels: Vec<(String, std::path::PathBuf)> = files
            .into_iter()
            .filter_map(|p| {
                let rel = p.strip_prefix(docs_root).ok()?.to_str()?.replace('\\', "/");
                Some((rel, p))
            })
            .collect();
        rels.sort();
        let mut sections = Vec::new();
        for (rel, path) in rels {
            sections.extend(self::sections(&rel, &std::fs::read_to_string(path)?));
        }
        Ok(DocIndex {
            format_version: DOCS_FORMAT_VERSION,
            framework_version: env!("CARGO_PKG_VERSION").to_owned(),
            source: SOURCE.to_owned(),
            sections,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("siderite-docs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn index_is_current() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        if !root.join(generate::SOURCE).is_dir() {
            // A packaged crate has no website sources to compare against.
            return;
        }
        let fresh = generate::build(&root.join(generate::SOURCE)).unwrap();
        let mut json = serde_json::to_string_pretty(&fresh).unwrap();
        json.push('\n');
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs-index.json");
        if std::env::var_os("SIDERITE_BLESS_DOCS").is_some() {
            std::fs::write(&file, &json).unwrap();
            return;
        }
        assert!(
            json == INDEX_JSON,
            "docs-index.json is stale; run `SIDERITE_BLESS_DOCS=1 cargo test -p \
             siderite-cli docs::tests::index_is_current`"
        );
        assert!(fresh.sections.len() > 50, "{}", fresh.sections.len());
        assert!(
            fresh
                .sections
                .iter()
                .all(|s| !s.path.contains("/contributing/") && !s.path.contains("/internals/"))
        );
    }

    #[test]
    fn sections_split_on_headings_outside_fences() {
        let md = "---\ntitle: Routing\ndescription: x\n---\n\nIntro.\n\n## Route `macros`\n\
                  Use `postgres` here.\n```rust\n## not a heading\n```\n### Exact expansion\nBody\n";
        let s = generate::sections("guides/http/routing.md", md);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].heading, "Routing");
        assert_eq!(s[0].url, "/siderite/guides/http/routing/");
        assert_eq!(s[0].line, 5);
        assert_eq!(s[1].heading, "Route macros");
        assert_eq!(s[1].line, 8);
        assert_eq!(s[1].url, "/siderite/guides/http/routing/#route-macros");
        assert_eq!(s[1].features, ["postgres"]);
        assert!(s[1].text.contains("## not a heading"));
        assert_eq!(s[2].path, "website/src/content/docs/guides/http/routing.md");
        assert_eq!(
            generate::slug("Cargo features & MSRV"),
            "cargo-features--msrv"
        );
    }

    #[test]
    fn nested_and_tilde_fences_and_duplicate_anchors() {
        let md = "---\ntitle: T\n---\n## Ex\n````md\n```rust\n## inner\n```\n````\n\
                  ~~~\n## tilde\n~~~\n## Ex\nb\n";
        let s = generate::sections("guides/t.md", md);
        let headings: Vec<_> = s.iter().map(|x| x.heading.as_str()).collect();
        assert_eq!(headings, ["Ex", "Ex"]);
        assert!(s[0].text.contains("## inner") && s[0].text.contains("## tilde"));
        assert_eq!(s[0].url, "/siderite/guides/t/#ex");
        assert_eq!(s[1].url, "/siderite/guides/t/#ex-1");
    }

    #[test]
    fn terms_match_at_word_starts_and_snippets_show_the_match() {
        assert_eq!(count("doctor error or order", "or"), 2);
        assert_eq!(count("routes route_x reroute", "route"), 2);
        let long = format!("{} needle tail", "word ".repeat(60));
        let snip = snippet(&long, &["needle".to_owned()]);
        assert!(snip.contains("needle"), "{snip}");
        assert!(snip.starts_with('…'));
        assert!(snip.chars().count() <= 162);
        assert_eq!(snippet("short line", &["x".to_owned()]), "short line");
    }

    fn index() -> DocIndex {
        let section = |heading: &str, path: &str, line, text: &str| DocSection {
            page: "Page".into(),
            heading: heading.into(),
            path: path.into(),
            line,
            url: "/u/".into(),
            features: vec![],
            text: text.into(),
        };
        DocIndex {
            format_version: 1,
            framework_version: "9.9.9".into(),
            source: "src".into(),
            sections: vec![
                section("Other", "b.md", 1, "query mentions routing once"),
                section("Routing", "a.md", 9, "query here"),
                section("Same", "a.md", 3, "query mentions routing once"),
                section("Nothing", "a.md", 1, "unrelated"),
            ],
        }
    }

    #[test]
    fn search_requires_every_term_and_orders_deterministically() {
        let hits = search(&index(), "Routing query", 10);
        let order: Vec<_> = hits.iter().map(|h| (h.path.as_str(), h.line)).collect();
        assert_eq!(order, [("a.md", 9), ("a.md", 3), ("b.md", 1)]);
        assert_eq!(hits[0].snippet, "query here");
        assert_eq!(search(&index(), "routing", 1).len(), 1);
        assert!(search(&index(), "routing absent", 10).is_empty());
        assert!(search(&index(), "  !! ", 10).is_empty());
    }

    #[test]
    fn embedded_index_finds_real_guides() {
        let index = embedded_index().unwrap();
        assert_eq!(index.framework_version, env!("CARGO_PKG_VERSION"));
        let hits = search(&index, "makemigrations", 5);
        assert!(!hits.is_empty());
        assert!(
            hits.iter()
                .all(|h| h.path.starts_with("website/src/content/docs/"))
        );
        let pg = search(&index, "PostgreSQL", 50);
        assert!(
            pg.iter()
                .any(|h| h.features.iter().any(|f| f == "postgres"))
        );
    }

    #[test]
    fn lock_versions_are_compared() {
        let dir = temp("lock");
        assert_eq!(version_check(&dir, "1.0.0").status, VersionStatus::Unknown);
        let lock = "version = 4\n\n[[package]]\nname = \"siderite-core\"\nversion = \"0.1.0\"\n\n\
                    [[package]]\nname = \"siderite\"\nversion = \"0.2.0\"\n";
        std::fs::write(dir.join("Cargo.lock"), lock).unwrap();
        let nested = dir.join("src");
        std::fs::create_dir_all(&nested).unwrap();
        let check = version_check(&nested, "0.2.0");
        assert_eq!(check.status, VersionStatus::Match);
        let check = version_check(&nested, "0.1.0");
        assert_eq!(check.status, VersionStatus::Mismatch);
        assert_eq!(check.project_version.as_deref(), Some("0.2.0"));
        let multi = "[[package]]\nversion = \"0.1.0\"\nname = \"siderite\"\n\n\
                     [[package]]\nname = \"siderite\"\nversion = \"0.2.0\"\n";
        assert_eq!(locked_siderite(multi), ["0.1.0", "0.2.0"]);
        std::fs::write(dir.join("Cargo.lock"), multi).unwrap();
        let check = version_check(&dir, "0.2.0");
        assert_eq!(check.status, VersionStatus::Match);
        assert_eq!(check.project_version.as_deref(), Some("0.2.0"));
        std::fs::write(dir.join("Cargo.lock"), "version = 4\n").unwrap();
        let check = version_check(&dir, "0.1.0");
        assert_eq!(check.status, VersionStatus::Unknown);
        assert!(check.reason.unwrap().contains("no `siderite`"));
    }

    #[test]
    fn mismatch_is_reported_in_text_and_json() {
        let dir = temp("mismatch");
        std::fs::write(
            dir.join("Cargo.lock"),
            "[[package]]\nname = \"siderite\"\nversion = \"0.0.1\"\n",
        )
        .unwrap();
        let report = search_report(&dir, "routing", 3).unwrap();
        assert_eq!(report.version_check.status, VersionStatus::Mismatch);
        let text = render(&report);
        assert!(
            text.contains("warning: these docs are for siderite"),
            "{text}"
        );
        assert!(text.contains("locks siderite 0.0.1"));
        assert!(mismatch_message(&report).is_some());
    }

    #[test]
    fn manifest_path_must_be_a_file() {
        let dir = temp("manifest");
        let global = GlobalArgs {
            manifest_path: Some(PathBuf::from("missing/Cargo.toml")),
            ..GlobalArgs::default()
        };
        let err = run(&dir, &global, &strings(&["docs", "search", "x"])).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("is not a file"));
    }

    #[test]
    fn parse_arguments() {
        let ok = |list: &[&str]| parse(&strings(list)).unwrap();
        assert_eq!(
            ok(&["docs", "search", "route", "macros"]),
            ("route macros".into(), 5)
        );
        assert_eq!(
            ok(&["docs", "search", "--limit", "2", "x"]),
            ("x".into(), 2)
        );
        assert_eq!(
            ok(&["docs", "search", "x", "--limit=50", "--json"]),
            ("x".into(), 50)
        );
        assert_eq!(
            ok(&["--limit", "3", "docs", "search", "x"]),
            ("x".into(), 3)
        );
        for bad in [
            &["docs"][..],
            &["docs", "find", "x"],
            &["docs", "search"],
            &["docs", "search", "!!"],
            &["docs", "search", "x", "--limit", "0"],
            &["docs", "search", "x", "--limit", "51"],
            &["docs", "search", "x", "--limit"],
            &["docs", "search", "x", "--bogus"],
            &["search", "x"],
        ] {
            let err = parse(&strings(bad)).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{bad:?}");
        }
    }
}
