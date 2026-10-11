//! The catalog against the source, lookups and the report shape.

#![allow(clippy::unwrap_used)]

use super::*;
use crate::fixtures::assert_schema_valid;
use std::collections::BTreeSet;
use std::path::Path;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

/// `ns.E001`-style codes in the non-test Rust sources under `dir`.
fn emitted(dir: &Path, found: &mut BTreeSet<String>) {
    for item in std::fs::read_dir(dir).unwrap().flatten() {
        let path = item.path();
        let name = item.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" && name != "explain" {
                emitted(&path, found);
            }
            continue;
        }
        let test_file = name.starts_with("tests") || name == "fixtures.rs";
        if !name.ends_with(".rs") || test_file {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        // Inline test modules come last.
        let text = text.split("#[cfg(test)]\nmod tests {").next();
        let text = text.unwrap_or("");
        for quoted in text.split('"').skip(1).step_by(2) {
            if is_code(quoted) {
                found.insert(quoted.to_owned());
            }
        }
    }
}

fn is_code(text: &str) -> bool {
    let Some((ns, id)) = text.split_once('.') else {
        return false;
    };
    let mut id = id.chars();
    let level = id.next().is_some_and(|c| "EWI".contains(c));
    let digits: String = id.collect();
    !ns.is_empty()
        && ns.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        && level
        && digits.len() == 3
        && digits.chars().all(|c| c.is_ascii_digit())
}

#[test]
fn the_catalog_covers_every_emitted_code_and_no_other() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found = BTreeSet::new();
    for krate in std::fs::read_dir(&crates).unwrap().flatten() {
        let src = krate.path().join("src");
        if src.is_dir() {
            emitted(&src, &mut found);
        }
    }
    found.insert("explain.E001".to_owned());
    let listed = CATALOG.iter().map(|e| e.code.to_owned());
    let listed: BTreeSet<String> = listed.collect();
    assert_eq!(listed, found);
    let codes: Vec<&str> = CATALOG.iter().map(|e| e.code).collect();
    let mut sorted = codes.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(codes, sorted, "sorted and unique");
}

#[test]
fn every_entry_is_complete_and_its_docs_exist() {
    let index = crate::docs::packaged_ids();
    for e in CATALOG {
        assert!(!e.title.is_empty() && !e.cause.is_empty(), "{}", e.code);
        assert!(!e.fix.is_empty(), "{}", e.code);
        assert!(e.verify.starts_with("siderite "), "{}", e.code);
        assert!(index.contains(e.docs), "{}: {}", e.code, e.docs);
    }
}

#[test]
fn looks_up_framework_compiler_and_lint_codes() {
    let found = lookup("models.e003").unwrap();
    assert_eq!(
        (found.code.as_str(), found.source),
        ("models.E003", Source::Siderite)
    );
    let rustc = lookup("e0308").unwrap();
    assert_eq!(rustc.code, "E0308");
    let url = rustc.url.unwrap();
    assert_eq!(url, "https://doc.rust-lang.org/error_codes/E0308.html");
    let lint = lookup("clippy::needless_return").unwrap();
    assert_eq!(lint.source, Source::Clippy);
    assert!(lint.url.unwrap().ends_with("#needless_return"));
    for unknown in ["models.E999", "E030", "E03081", "clippy::", "nope"] {
        assert!(lookup(unknown).is_none(), "{unknown}");
    }
}

#[test]
fn parses_one_code_or_the_list() {
    let one = parse(&args(&["explain", "E0308", "--json"])).unwrap();
    assert_eq!(one.code.as_deref(), Some("E0308"));
    assert!(one.json);
    assert_eq!(parse(&args(&["explain", "--list"])).unwrap().code, None);
    for bad in [
        &["explain"][..],
        &["explain", "a", "b"],
        &["explain", "--list", "a"],
        &["explain", "a", "--all"],
    ] {
        assert!(parse(&args(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn reports_validate_and_unknown_codes_fail() {
    for code in ["verify.E001", "E0308", "clippy::unwrap_used"] {
        let options = Options {
            code: Some(code.to_owned()),
            json: true,
        };
        let report = report(&options);
        assert!(report.ok, "{code}");
        assert_schema_valid(&serde_json::to_value(&report).unwrap());
    }
    let unknown = report(&Options {
        code: Some("x.E123".to_owned()),
        json: false,
    });
    assert_eq!(unknown.exit_code(), 1);
    assert!(unknown.data.entries.is_empty());
    assert_eq!(unknown.diagnostics[0].code(), "explain.E001");
    assert_schema_valid(&serde_json::to_value(&unknown).unwrap());
    let all = report(&Options {
        code: None,
        json: false,
    });
    assert_eq!(all.data.entries.len(), CATALOG.len());
    let table = render(&all, true);
    assert!(table.contains("models.E003      Model without"), "{table}");
    let one = report(&Options {
        code: Some("models.E003".to_owned()),
        json: false,
    });
    let text = render(&one, false);
    assert!(text.starts_with("models.E003: Model without a primary key"));
    assert!(text.contains("Verify: siderite check\n"), "{text}");
}
