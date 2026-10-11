//! The packaged index, search, version checks and the report shape.

#![allow(clippy::unwrap_used)]

use super::index::{Entry, Kind, packaged, search, terms};
use super::version::locked;
use super::*;
use crate::fixtures::{ScratchDir, assert_schema_valid};

const UPDATE: &str = "SIDERITE_UPDATE_DOCS_INDEX";

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

#[test]
fn packaged_index_matches_its_sources() {
    let Some(root) = generate::repo_root() else {
        return; // A packaged crate has no website sources.
    };
    let built = generate::build(&root);
    let rendered = serde_json::to_string_pretty(&built).unwrap() + "\n";
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = crate_dir.join("src/docs/index.json");
    if std::env::var_os(UPDATE).is_some() {
        std::fs::write(&path, &rendered).unwrap();
        return;
    }
    let packaged = std::fs::read_to_string(&path).unwrap();
    assert!(
        packaged == rendered,
        "src/docs/index.json is stale; run {UPDATE}=1 cargo test -p \
         siderite-cli docs"
    );
    assert!(built.entries.len() > 50, "{}", built.entries.len());
    let ids: std::collections::HashSet<&str> =
        built.entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids.len(), built.entries.len(), "ids are unique");
}

#[test]
fn the_packaged_index_is_this_version() {
    let index = packaged().unwrap();
    assert_eq!(index.siderite, env!("CARGO_PKG_VERSION"));
    assert!(index.entries.iter().any(|e| e.kind == Kind::Example));
    assert!(
        !index
            .entries
            .iter()
            .any(|e| e.path.contains("contributing"))
    );
}

fn entry(id: &str, title: &str, text: &str) -> Entry {
    Entry {
        id: id.to_owned(),
        kind: Kind::Guide,
        title: title.to_owned(),
        path: format!("{id}.md"),
        line: 1,
        features: Vec::new(),
        text: text.to_owned(),
    }
}

fn small() -> Index {
    Index {
        format: 1,
        siderite: "9.9.9".to_owned(),
        entries: vec![
            entry("a", "Migrations", "apply with migrate; rollback undoes"),
            entry("b", "Routing", "routes and migrate are words here"),
            entry("c", "Cache", "redis cache"),
            entry("d", "Rollback", "rollback rollback rollback"),
        ],
    }
}

#[test]
fn search_prefers_all_terms_then_titles_and_breaks_ties_by_id() {
    let index = small();
    let ids = |q: &str| -> Vec<String> {
        let found = search(&index, &terms(q));
        found.iter().map(|s| s.entry.id.clone()).collect()
    };
    assert_eq!(ids("migrate rollback"), ["a"]);
    assert_eq!(ids("Rollback"), ["d", "a"]);
    assert_eq!(ids("redis or nothing"), ["c"]);
    assert!(ids("zzz").is_empty());
    assert!(ids("").is_empty());
}

#[test]
fn real_queries_find_their_guides() {
    let index = packaged().unwrap();
    let top = |q: &str| {
        let found = search(&index, &terms(q));
        found
            .first()
            .map(|s| s.entry.path.clone())
            .unwrap_or_default()
    };
    let migrations = top("migrate rollback");
    assert!(migrations.ends_with("migrations.md"), "{migrations}");
    let redis = top("redis cache");
    assert!(redis.ends_with("backends.md"), "{redis}");
    assert!(top("Depends dependency injection").contains("di.md"));
}

#[test]
fn parses_queries_and_limits() {
    let parsed = parse(&args(&["docs", "search", "query", "set", "--json"]));
    let parsed = parsed.unwrap();
    assert_eq!(parsed.query, "query set");
    assert!(parsed.json);
    assert_eq!(parsed.limit, DEFAULT_LIMIT);
    let limited = parse(&args(&["docs", "search", "--limit", "2", "x"]));
    assert_eq!(limited.unwrap().limit, 2);
    let inline = parse(&args(&["docs", "search", "x", "--limit=50"]));
    assert_eq!(inline.unwrap().limit, 50);
    for bad in [
        &["docs"][..],
        &["docs", "find", "x"],
        &["docs", "search"],
        &["docs", "search", "--", "!!"],
        &["docs", "search", "x", "--limit", "0"],
        &["docs", "search", "x", "--limit=51"],
        &["docs", "search", "x", "--full"],
    ] {
        assert!(parse(&args(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn reads_locked_versions() {
    let lock = "[[package]]\nname = \"siderite\"\nversion = \"0.2.0\"\n\n\
                [[package]]\nname = \"siderite-core\"\nversion = \"0.9.0\"\n\n\
                [[package]]\nname = \"siderite\"\nversion = \"0.1.0\"\n";
    assert_eq!(locked(lock, "siderite"), ["0.1.0", "0.2.0"]);
    assert!(locked("", "siderite").is_empty());
}

#[test]
fn a_version_mismatch_is_a_warning() {
    let scratch = ScratchDir::new("docs-version");
    let lock = "[[package]]\nname = \"siderite\"\nversion = \"0.0.1\"\n";
    std::fs::write(scratch.path().join("Cargo.lock"), lock).unwrap();
    let nested = scratch.path().join("src");
    std::fs::create_dir_all(&nested).unwrap();
    let options = parse(&args(&["docs", "search", "rollback"])).unwrap();
    let report = report(&small(), &options, &nested);
    assert!(report.ok);
    assert_eq!(report.data.installed, ["0.0.1"]);
    let codes = report.diagnostics.iter().map(Diagnostic::code);
    assert_eq!(codes.collect::<Vec<_>>(), ["docs.W001"]);
    assert!(mismatch("1.0.0", &["1.0.0".to_owned()]).is_none());
    assert!(mismatch("1.0.0", &[]).is_none());
}

#[test]
fn results_carry_versioned_sources_and_validate() {
    let scratch = ScratchDir::new("docs-report");
    let options = parse(&args(&["docs", "search", "rollback"])).unwrap();
    let report = report(&small(), &options, scratch.path());
    assert_eq!(report.command, "docs search");
    assert_eq!(report.data.total, 2);
    let first = &report.data.results[0];
    assert_eq!(first.id, "d");
    assert!(first.source_url.ends_with("/blob/v9.9.9/d.md#L1"));
    assert_eq!(first.excerpt, ["rollback rollback rollback"]);
    assert!(!first.truncated);
    assert_schema_valid(&serde_json::to_value(&report).unwrap());
    let text = render(&report);
    assert!(
        text.starts_with("siderite 9.9.9 docs: 2 of 2 matches"),
        "{text}"
    );
    assert!(text.contains("  d.md:1\n"), "{text}");
}
