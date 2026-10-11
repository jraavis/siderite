//! Upgrade previews: arguments, sources, semver and release notes.

#![allow(clippy::unwrap_used)]

use super::notes::{Semver, semver};
use super::*;
use crate::docs::index::{Entry, Index, Kind};
use crate::fixtures::assert_schema_valid;
use serde_json::{Value, json};

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

#[test]
fn arguments() {
    let words = ["upgrade", "--to", "v0.3.0", "--dry-run", "--json"];
    let parsed = parse(&args(&words)).unwrap();
    assert_eq!((parsed.to.as_str(), parsed.json), ("0.3.0", true));
    for bad in [
        &["upgrade", "--to", "0.3.0"][..],
        &["upgrade", "--dry-run"],
        &["upgrade", "--to", "latest", "--dry-run"],
        &["upgrade", "--to", "0.3.0", "--dry-run", "--write"],
    ] {
        assert!(parse(&args(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn semver_lines() {
    assert_eq!(semver("0.1.2", "0.1.9"), Semver::Compatible);
    assert_eq!(semver("0.1.2", "0.2.0"), Semver::Breaking);
    assert_eq!(semver("1.4.0", "1.9.0"), Semver::Compatible);
    assert_eq!(semver("1.4.0", "2.0.0"), Semver::Breaking);
    assert_eq!(semver("0.2.0", "0.1.0"), Semver::Downgrade);
    assert_eq!(semver("", "0.1.0"), Semver::Unknown);
}

const MANIFEST: &str = "[dependencies]\n\
    siderite = { workspace = true }\n\
    siderite-cli = { git = \"https://github.com/o/s\", branch = \"main\" }\n\
    siderite-testkit = { path = \"../kit\" }\n\
    serde = \"1\"\n";

const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

fn specs() -> Vec<Value> {
    vec![
        json!({ "name": "siderite", "req": "^0.1",
            "source": REGISTRY }),
        json!({ "name": "siderite-cli", "req": "*",
            "source": "git+https://github.com/o/s?branch=main" }),
        json!({ "name": "siderite-testkit", "req": "*", "path": "/w/kit" }),
        json!({ "name": "serde", "req": "^1" }),
    ]
}

fn index(notes: Option<&str>) -> Index {
    let entries = notes.map(|text| Entry {
        id: "changelog#0.2.0".to_owned(),
        kind: Kind::Changelog,
        title: "Changelog > 0.2.0".to_owned(),
        path: "CHANGELOG.md".to_owned(),
        line: 3,
        features: Vec::new(),
        text: text.to_owned(),
    });
    Index {
        format: 1,
        siderite: "0.2.0".to_owned(),
        entries: entries.into_iter().collect(),
    }
}

fn project(specs: &[Value]) -> Project<'_> {
    Project {
        locked: vec!["0.1.4".to_owned()],
        specs,
        manifests: Manifests {
            text: MANIFEST,
            package: "/w/app/Cargo.toml",
            root: "/w/Cargo.toml",
        },
    }
}

#[test]
fn each_source_is_handled_explicitly() {
    let specs = specs();
    let report = report(&project(&specs), "0.2.0", &index(None));
    let deps = &report.data.dependencies;
    assert_eq!(deps.len(), 3, "serde is not the framework");
    assert_eq!(deps[0].source, SourceKind::Registry);
    assert!(deps[0].inherited);
    assert_eq!(deps[0].manifest, "/w/Cargo.toml");
    assert_eq!(deps[0].proposed.as_deref(), Some("version \"0.2.0\""));
    assert_eq!(deps[1].source, SourceKind::Git);
    assert_eq!(deps[1].current, "git https://github.com/o/s (branch=main)");
    assert_eq!(deps[1].proposed.as_deref(), Some("tag = \"v0.2.0\""));
    assert_eq!(
        (deps[2].source, deps[2].proposed.clone()),
        (SourceKind::Path, None)
    );
    assert_eq!(deps[2].manifest, "/w/app/Cargo.toml");
    let codes = report.diagnostics.iter().map(Diagnostic::code);
    let codes: Vec<&str> = codes.collect();
    assert_eq!(codes, ["upgrade.W001", "upgrade.W002"]);
    assert!(!report.data.compatibility.notes_known);
    assert_eq!(report.data.compatibility.semver, Semver::Breaking);
    assert_eq!(report.exit_code(), 0, "a preview never fails");
    assert_schema_valid(&serde_json::to_value(&report).unwrap());
    let text = render(&report);
    assert!(text.contains("siderite 0.1.4 -> 0.2.0 (dry run"), "{text}");
    assert!(text.contains("registry, inherited"), "{text}");
    assert!(text.contains("openapi diff"), "{text}");
}

#[test]
fn release_notes_for_the_target_are_reported() {
    let notes = "### Added\n- **CLI:** new thing.\n\n### Changed\n\
                 - **ORM:** `save` returns the row,\n  not unit.\n\
                 ### Removed\n- `old_api`.\n";
    let specs = specs();
    let report = report(&project(&specs), "0.2.0", &index(Some(notes)));
    let compatibility = &report.data.compatibility;
    assert!(compatibility.notes_known);
    let want = ["**ORM:** `save` returns the row, not unit.", "`old_api`."];
    assert_eq!(compatibility.notes, want);
    assert!(
        report
            .diagnostics
            .iter()
            .all(|d| d.code() != "upgrade.W001")
    );
}

#[test]
fn inheritance_spellings() {
    use super::plan::inherited;
    assert!(inherited("siderite.workspace = true", "siderite"));
    assert!(inherited(
        "siderite = { workspace=true, features = [] }",
        "siderite"
    ));
    assert!(!inherited(
        "siderite-cli = { workspace = true }",
        "siderite"
    ));
    assert!(!inherited("siderite = \"0.1\"", "siderite"));
}
