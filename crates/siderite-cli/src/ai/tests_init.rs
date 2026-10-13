//! `ai init`: one owned context file, refreshed without losing user text.

#![allow(clippy::unwrap_used)]

use super::init::{Init, InitOptions, PATH, link_plan, parse, run};
use crate::fixtures::{ScratchDir, author};
use crate::report::snapshot::{Limits, Snapshot, snapshot};
use crate::settings::CliSettings;
use siderite_core::{App, get};
use std::path::Path;

fn snap(extra_route: bool) -> Snapshot {
    let mut app = App::new()
        .title("Shop")
        .route("/health", get(|| async { "ok" }).operation_id("health"));
    if extra_route {
        let items = get(|| async { "[]" }).operation_id("items");
        app = app.route("/items", items);
    }
    let settings = CliSettings::new().database("default", "sqlite::memory:");
    snapshot(&app, &[author()], &settings, Limits::default()).data
}

fn read(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(PATH)).unwrap()
}

fn opts(check: bool, dry_run: bool, force: bool) -> Init {
    let options = InitOptions {
        check,
        dry_run,
        force,
    };
    Init {
        options,
        links: Vec::new(),
    }
}

#[test]
fn creates_once_then_reports_current() {
    let scratch = ScratchDir::new("ai-init");
    let dir = scratch.path();
    assert_eq!(
        run(dir, &snap(false), &opts(true, false, false)).unwrap(),
        1
    );
    assert!(!dir.join(PATH).exists(), "--check wrote");
    assert_eq!(
        run(dir, &snap(false), &opts(false, true, false)).unwrap(),
        0
    );
    assert!(!dir.join(PATH).exists(), "--dry-run wrote");
    run(dir, &snap(false), &Init::default()).unwrap();
    let text = read(dir);
    let version = env!("CARGO_PKG_VERSION");
    let begin = "<!-- siderite:context begin";
    let marker = format!("{begin} format=1 siderite={version} hash=");
    assert!(text.starts_with(&marker), "{text}");
    assert!(text.contains("# Siderite context: Shop"));
    assert!(text.contains("| GET | /health | health |"));
    assert!(text.contains("### Author (table `authors`)"));
    assert!(text.contains("```rust\nuse siderite::prelude::*;"));
    assert!(text.contains("## Project notes"));
    // A rerun is a no-op, and --check agrees.
    run(dir, &snap(false), &Init::default()).unwrap();
    assert_eq!(read(dir), text);
    assert_eq!(
        run(dir, &snap(false), &opts(true, false, false)).unwrap(),
        0
    );
}

#[test]
fn a_stale_block_is_refreshed_and_user_text_kept() {
    let scratch = ScratchDir::new("ai-init-stale");
    let dir = scratch.path();
    run(dir, &snap(false), &Init::default()).unwrap();
    let mine = format!("My intro.\n{}\nMy own notes.\n", read(dir));
    std::fs::write(dir.join(PATH), &mine).unwrap();
    // The project gained a route: stale, and --check says so.
    assert_eq!(run(dir, &snap(true), &opts(true, false, false)).unwrap(), 1);
    assert_eq!(read(dir), mine, "--check wrote");
    run(dir, &snap(true), &Init::default()).unwrap();
    let text = read(dir);
    assert!(text.starts_with("My intro.\n<!-- siderite:context begin"));
    assert!(text.ends_with("\nMy own notes.\n"), "{text}");
    assert!(text.contains("| GET | /items | items |"));
}

#[test]
fn another_framework_version_is_stale() {
    let scratch = ScratchDir::new("ai-init-version");
    let dir = scratch.path();
    run(dir, &snap(false), &Init::default()).unwrap();
    let version = format!("siderite={}", env!("CARGO_PKG_VERSION"));
    let old = read(dir).replacen(&version, "siderite=0.0.1", 1);
    std::fs::write(dir.join(PATH), &old).unwrap();
    assert_eq!(
        run(dir, &snap(false), &opts(true, false, false)).unwrap(),
        1
    );
    run(dir, &snap(false), &Init::default()).unwrap();
    assert!(read(dir).contains(&version));
}

#[test]
fn hand_edits_inside_the_block_are_not_overwritten() {
    let scratch = ScratchDir::new("ai-init-edited");
    let dir = scratch.path();
    run(dir, &snap(false), &Init::default()).unwrap();
    let edited = read(dir).replacen("## Commands", "## My commands", 1);
    std::fs::write(dir.join(PATH), &edited).unwrap();
    let err = run(dir, &snap(false), &Init::default()).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("edited by hand"), "{text}");
    assert!(text.contains("-## My commands"), "{text}");
    assert_eq!(err.exit_code(), 1);
    assert_eq!(read(dir), edited);
    run(dir, &snap(false), &opts(false, false, true)).unwrap();
    assert!(read(dir).contains("\n## Commands\n"));
}

#[test]
fn a_file_without_markers_is_left_alone() {
    let scratch = ScratchDir::new("ai-init-foreign");
    let dir = scratch.path();
    std::fs::create_dir_all(dir.join(".siderite")).unwrap();
    std::fs::write(dir.join(PATH), "mine\n").unwrap();
    let err = run(dir, &snap(false), &opts(false, false, true)).unwrap_err();
    assert!(err.to_string().contains("no siderite markers"), "{err}");
    assert_eq!(read(dir), "mine\n");
}

#[cfg(unix)]
#[test]
fn a_symlinked_directory_is_refused() {
    let scratch = ScratchDir::new("ai-init-link");
    let outside = ScratchDir::new("ai-init-outside");
    let dir = scratch.path();
    std::os::unix::fs::symlink(outside.path(), dir.join(".siderite")).unwrap();
    assert!(run(dir, &snap(false), &Init::default()).is_err());
    assert!(!outside.path().join("context.md").exists());
}

#[test]
fn links_are_written_only_when_asked_and_keep_user_text() {
    let scratch = ScratchDir::new("ai-links");
    let repo = scratch.path();
    std::fs::create_dir(repo.join(".git")).unwrap();
    let dir = repo.join("app");
    std::fs::create_dir(&dir).unwrap();
    let agents = repo.join("AGENTS.md");
    std::fs::write(&agents, "# House rules\nNo panics.").unwrap();
    run(&dir, &snap(false), &Init::default()).unwrap();
    let untouched = std::fs::read_to_string(&agents).unwrap();
    assert_eq!(untouched, "# House rules\nNo panics.", "not by default");
    let words = ["ai", "init", "--link", "AGENTS.md"];
    let words: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
    let init = parse(&words).unwrap();
    run(&dir, &snap(false), &init).unwrap();
    let linked = std::fs::read_to_string(&agents).unwrap();
    assert!(linked.starts_with("# House rules\nNo panics.\n\n"));
    assert!(linked.contains("](app/.siderite/context.md)"), "{linked}");
    run(&dir, &snap(false), &init).unwrap();
    assert_eq!(std::fs::read_to_string(&agents).unwrap(), linked);
    let mut check = init.clone();
    check.options.check = true;
    assert_eq!(run(&dir, &snap(false), &check).unwrap(), 0);
    std::fs::write(&agents, format!("{linked}{linked}")).unwrap();
    let err = run(&dir, &snap(false), &init).unwrap_err();
    assert!(err.to_string().contains("ai.E002"), "{err}");
    let bad = ["ai", "init", "--link", "../x.md"];
    let bad: Vec<String> = bad.iter().map(|w| (*w).to_owned()).collect();
    assert!(parse(&bad).is_err());
}

#[test]
fn each_link_reports_its_own_state() {
    let scratch = ScratchDir::new("ai-links-state");
    let repo = scratch.path();
    std::fs::create_dir(repo.join(".git")).unwrap();
    let files = ["AGENTS.md".to_owned(), "docs/AI.md".to_owned()];
    let (plan, _) = link_plan(repo, &files[1..]).unwrap();
    plan.apply().unwrap();
    let (_, stale) = link_plan(repo, &files).unwrap();
    assert_eq!(stale, ["AGENTS.md"], "the current file stays current");
}
