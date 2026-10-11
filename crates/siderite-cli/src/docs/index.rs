//! The packaged documentation index and keyword search over it.
//!
//! `index.json` is generated from the maintained public documentation
//! (the website's start, guides, reference and tutorial pages) and from
//! the compiled examples, and is checked against those sources by a test,
//! so it always matches this release. Search is local: keyword scoring
//! with a fixed tie-break, so the same query gives the same results.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Version of the index layout.
pub(crate) const INDEX_FORMAT: u32 = 1;

/// The generated index, packaged with this crate.
const INDEX: &str = include_str!("index.json");

/// What an entry documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    /// A section of a documentation page.
    Guide,
    /// An example program, compiled in the framework's own build.
    Example,
}

/// One searchable section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// Stable id: page path and heading anchor.
    pub id: String,
    /// What it is.
    pub kind: Kind,
    /// Page title, then the section heading.
    pub title: String,
    /// Source file, relative to the repository root.
    pub path: String,
    /// First line of the section in that file.
    pub line: u32,
    /// Cargo features the section says it needs.
    pub features: Vec<String>,
    /// The section's Markdown (or the example's source).
    pub text: String,
}

/// The whole index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Index {
    /// [`INDEX_FORMAT`].
    pub format: u32,
    /// The framework version the index documents.
    pub siderite: String,
    /// Every entry, in source order.
    pub entries: Vec<Entry>,
}

/// The packaged index.
///
/// Errors:
///   A message when the packaged file does not parse or has another
///   layout version (a build defect).
pub(crate) fn packaged() -> Result<Index, String> {
    let parsed = serde_json::from_str::<Index>(INDEX);
    let index = parsed.map_err(|e| format!("docs index: {e}"))?;
    if index.format != INDEX_FORMAT {
        let found = index.format;
        return Err(format!("docs index: format {found}, not {INDEX_FORMAT}"));
    }
    Ok(index)
}

/// Lowercase words of letters, digits and `_`.
pub(crate) fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

/// The distinct words of `query`.
pub(crate) fn terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = words(query).collect();
    terms.sort();
    terms.dedup();
    terms
}

/// An entry and its score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Scored<'a> {
    /// The entry.
    pub entry: &'a Entry,
    /// Higher is better.
    pub score: f64,
}

/// A title word counts like this many body words at most.
const TITLE_WEIGHT: f64 = 2.0;
/// BM25 term-frequency saturation.
const K1: f64 = 1.2;
/// BM25 length normalization: long sections need more occurrences.
const B: f64 = 0.75;

/// Entries matching every term (or, when none does, any term), ranked
/// with BM25 over the section text plus a bonus for terms in the title.
/// Ties are broken by id.
pub(crate) fn search<'a>(index: &'a Index, terms: &[String]) -> Hits<'a> {
    if terms.is_empty() {
        return Vec::new();
    }
    let slots: Slots = terms
        .iter()
        .enumerate()
        .map(|(i, t)| (t.as_str(), i))
        .collect();
    let counts: Vec<Counts> = index
        .entries
        .iter()
        .map(|e| Counts::of(e, &slots))
        .collect();
    let stats = Stats::new(&counts, terms.len());
    let all = |c: &Counts| (0..terms.len()).all(|t| c.matches(t));
    let require_all = counts.iter().any(all);
    let mut hits: Hits = index
        .entries
        .iter()
        .zip(&counts)
        .filter(|(_, c)| if require_all { all(c) } else { c.any() })
        .map(|(entry, c)| Scored {
            entry,
            score: c.score(&stats),
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.entry.id.cmp(&b.entry.id))
    });
    hits
}

type Hits<'a> = Vec<Scored<'a>>;
/// Excerpt lines.
pub(crate) type Lines = Vec<String>;
/// Query term to its position.
type Slots<'a> = HashMap<&'a str, usize>;

/// Term occurrences in one entry's title and body, and the body length.
struct Counts {
    title: Vec<u32>,
    body: Vec<u32>,
    length: f64,
}

/// Collection statistics: each term's rarity and the mean body length.
struct Stats {
    rarity: Vec<f64>,
    mean_length: f64,
}

impl Stats {
    fn new(counts: &[Counts], terms: usize) -> Self {
        let total = counts.len() as f64;
        let rarity = (0..terms)
            .map(|t| {
                let df = counts.iter().filter(|c| c.matches(t)).count();
                let df = df as f64;
                (1.0 + (total - df + 0.5) / (df + 0.5)).ln()
            })
            .collect();
        let lengths: f64 = counts.iter().map(|c| c.length).sum();
        let mean_length = (lengths / total.max(1.0)).max(1.0);
        Self {
            rarity,
            mean_length,
        }
    }
}

impl Counts {
    fn of(entry: &Entry, slots: &Slots) -> Self {
        let (title, _) = tally(&entry.title, slots);
        let (body, length) = tally(&entry.text, slots);
        Self {
            title,
            body,
            length,
        }
    }

    fn matches(&self, term: usize) -> bool {
        self.title[term] + self.body[term] > 0
    }

    fn any(&self) -> bool {
        (0..self.title.len()).any(|t| self.matches(t))
    }

    fn score(&self, stats: &Stats) -> f64 {
        let norm = K1 * (1.0 - B + B * self.length / stats.mean_length);
        let mut score = 0.0;
        for (t, rarity) in stats.rarity.iter().enumerate() {
            let tf = f64::from(self.body[t]);
            let body = tf * (K1 + 1.0) / (tf + norm);
            let title = TITLE_WEIGHT * f64::from(self.title[t].min(1));
            score += rarity * (body + title);
        }
        score
    }
}

/// Occurrences of each slot's term in `text`, and its word count.
fn tally(text: &str, slots: &Slots) -> (Vec<u32>, f64) {
    let mut found = vec![0u32; slots.len()];
    let mut length = 0u32;
    for word in words(text) {
        length += 1;
        if let Some(&i) = slots.get(word.as_str()) {
            found[i] += 1;
        }
    }
    (found, f64::from(length))
}

/// Up to `max` lines of `text` containing a term, trimmed.
pub(crate) fn excerpt(text: &str, terms: &[String], max: usize) -> Lines {
    text.lines()
        .filter(|line| words(line).any(|w| terms.contains(&w)))
        .map(|line| clip(line.trim(), 120))
        .take(max)
        .collect()
}

/// `text` cut to at most `max` characters, marked with `...` when cut.
pub(crate) fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_owned(),
    }
}
