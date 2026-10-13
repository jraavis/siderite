//! `ai context --task`: selection, the byte budget and file inclusion.

#![allow(clippy::unwrap_used)]

use super::files::collect;
use super::task::{self, Options, Sources, TaskContext};
use super::*;
use crate::docs::index::packaged;
use crate::fixtures::{ScratchDir, assert_schema_valid, author, book};
use crate::report::snapshot::{Limits, snapshot};
use siderite_core::get;
use std::path::PathBuf;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

fn app() -> App {
    App::new()
        .title("Shop")
        .route("/health", get(|| async { "ok" }).operation_id("health"))
        .route("/authors", get(|| async { "[]" }).operation_id("authors"))
}

fn options(task: &str, budget: usize) -> Options {
    Options {
        task: task.to_owned(),
        budget,
        include: Vec::new(),
        format: Format::Markdown,
    }
}

fn context(o: &Options, root: &Path) -> TaskContext {
    let settings = CliSettings::new().database("default", "sqlite::memory:");
    let models = [author(), book()];
    let snap = snapshot(&app(), &models, &settings, Limits::default());
    let index = packaged().unwrap();
    let sources = Sources {
        snapshot: &snap.data,
        notes: Vec::new(),
        index: &index,
        root,
    };
    task::build(&sources, o)
}

#[test]
fn parses_task_budget_and_includes() {
    let rest = args(&[
        "ai",
        "context",
        "--task",
        "add a filter",
        "--budget=5000",
        "--include",
        "src",
        "--include",
        "Cargo.toml",
    ]);
    let parsed = task::parse(&rest, true).unwrap();
    assert_eq!(parsed.task, "add a filter");
    assert_eq!(parsed.budget, 5000);
    assert_eq!(parsed.include.len(), 2);
    assert_eq!(parsed.format, Format::Json);
    let plain = args(&["ai", "context", "--task", "x"]);
    let defaults = task::parse(&plain, false);
    assert_eq!(defaults.unwrap().budget, task::DEFAULT_BUDGET);
    for bad in [
        &["ai", "context"][..],
        &["ai", "context", "--task", " "],
        &["ai", "context", "--task", "x", "--budget", "100"],
        &["ai", "context", "--task", "x", "--include"],
        &["ai", "context", "--task", "x", "--live"],
    ] {
        assert!(task::parse(&args(bad), false).is_err(), "{bad:?}");
    }
}

#[test]
fn selects_matching_models_routes_and_docs() {
    let scratch = ScratchDir::new("task-select");
    let o = options("list authors by name", 32 << 10);
    let ctx = context(&o, scratch.path());
    let kinds: Vec<&str> = ctx.parts.iter().map(|p| p.kind).collect();
    assert_eq!(&kinds[..3], ["project", "models", "routes"]);
    let models = &ctx.parts[1];
    // Book has an `author` foreign key; Author matches more words.
    assert_eq!(models.title, "Models (2 of 2 match the task)");
    let author_at = models.text.find("### Author").unwrap();
    assert!(author_at < models.text.find("### Book").unwrap());
    let o = options("rename the name field", 32 << 10);
    let renamed = context(&o, scratch.path());
    let one = &renamed.parts[1];
    assert_eq!(one.title, "Models (1 of 2 match the task)");
    assert!(one.text.contains("Other models: Book."), "{}", one.text);
    let routes = &ctx.parts[2];
    assert!(routes.text.contains("/authors"), "{}", routes.text);
    assert!(!routes.text.contains("/health |"), "{}", routes.text);
    assert!(kinds.contains(&"docs"), "{kinds:?}");
    let docs = ctx.parts.iter().find(|p| p.kind == "docs").unwrap();
    assert!(
        docs.source
            .contains(&format!("(siderite {} docs)", ctx.docs_version))
    );
    assert!(ctx.missing.iter().any(|m| m.starts_with("source files")));
    assert!(
        ctx.markdown
            .starts_with("# Context for: list authors by name")
    );
}

#[test]
fn the_budget_holds_and_the_output_is_deterministic() {
    let scratch = ScratchDir::new("task-budget");
    for budget in [4096, 5000, 9000, 32 << 10] {
        let o = options("migrations rollback transaction", budget);
        let first = context(&o, scratch.path());
        assert!(
            first.markdown.len() <= budget,
            "{budget}: {}",
            first.markdown.len()
        );
        assert_eq!(first.budget.used, first.markdown.len());
        assert_eq!(first, context(&o, scratch.path()), "{budget}");
    }
    let tight = context(&options("migrations rollback", 4096), scratch.path());
    let cut = tight.parts.iter().any(|p| p.truncated);
    assert!(cut || !tight.omitted.is_empty());
    assert!(tight.markdown.contains("## Not included"));
    assert!(tight.markdown.contains("- omitted ("), "{}", tight.markdown);
}

#[test]
fn reports_validate() {
    let scratch = ScratchDir::new("task-schema");
    let ctx = context(&options("authors", 8192), scratch.path());
    let report = Envelope::new("ai context", ctx, Vec::new());
    assert_schema_valid(&serde_json::to_value(&report).unwrap());
}

#[test]
fn includes_only_project_files_that_are_safe_to_share() {
    let scratch = ScratchDir::new("task-files");
    let root = scratch.path().join("app");
    let write = |rel: &str, bytes: &[u8]| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    };
    write(
        "src/main.rs",
        b"// db: postgres://u:hunter2@h/db\nfn main() {}\n",
    );
    write(".env", b"DATABASE_URL=x");
    write("target/debug/build.rs", b"// build output");
    write(".cache/x.rs", b"// hidden");
    write("src/blob.bin", &[0xff, 0xfe, 0x00]);
    std::fs::create_dir_all(scratch.path().join("other")).unwrap();
    let requested = [
        PathBuf::from("."),
        PathBuf::from("../other"),
        PathBuf::from("missing.rs"),
    ];
    let got = collect(&root, &requested);
    let paths = got.included.iter().map(|f| f.path.as_str());
    let paths: Vec<&str> = paths.collect();
    assert_eq!(paths, ["src/main.rs"]);
    assert!(!got.included[0].text.contains("hunter2"));
    let refused: Vec<(&str, &str)> = got
        .refused
        .iter()
        .map(|r| (r.path.as_str(), r.reason))
        .collect();
    assert!(
        refused.contains(&("../other", "outside the project")),
        "{refused:?}"
    );
    assert!(
        refused.contains(&("missing.rs", "not found")),
        "{refused:?}"
    );
    assert!(
        refused.contains(&(".env", "may hold secrets")),
        "{refused:?}"
    );
    assert!(
        refused.contains(&("src/blob.bin", "not text")),
        "{refused:?}"
    );
}

#[test]
fn git_ignore_rules_apply() {
    let git_ok = std::process::Command::new("git").arg("--version").output();
    if !git_ok.is_ok_and(|o| o.status.success()) {
        return;
    }
    let scratch = ScratchDir::new("task-git");
    let root = scratch.path();
    let init = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(init.success());
    std::fs::write(root.join(".gitignore"), "ignored.rs\n").unwrap();
    std::fs::write(root.join("ignored.rs"), "// no").unwrap();
    std::fs::write(root.join("kept.rs"), "// yes").unwrap();
    let requested = [PathBuf::from("ignored.rs"), PathBuf::from(".")];
    let got = collect(root, &requested);
    let paths = got.included.iter().map(|f| f.path.as_str());
    let paths: Vec<&str> = paths.collect();
    assert_eq!(paths, [".gitignore", "kept.rs"]);
    assert_eq!(got.refused[0].reason, "ignored by git");
}

#[test]
fn a_part_whose_heading_is_too_long_is_omitted() {
    let scratch = ScratchDir::new("task-heading");
    let root = scratch.path();
    // The path is in both the heading and the source line.
    let part = "d".repeat(240);
    let deep = format!("{part}/{part}/{part}/{part}/{part}/x.rs");
    std::fs::create_dir_all(root.join(&deep).parent().unwrap()).unwrap();
    std::fs::write(root.join(&deep), "fn x() {}\n".repeat(400)).unwrap();
    let mut o = options("authors", 4096);
    o.include = vec![PathBuf::from(&deep)];
    let ctx = context(&o, root);
    assert!(ctx.markdown.len() <= 4096, "{}", ctx.markdown.len());
    assert!(
        ctx.omitted.iter().any(|p| p.kind == "file"),
        "{:?}",
        ctx.omitted
    );
}
