//! `siderite` project selection against workspaces built on disk.
//!
//! Only `cargo metadata` runs (no build, no network), in a directory
//! whose name contains a space.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("siderite sel {id}-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn siderite_path() -> String {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    crates.join("siderite").display().to_string()
}

fn write_package(dir: &Path, name: &str, bins: &[&str]) {
    fs::create_dir_all(dir.join("src/bin")).unwrap();
    let manifest = format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\
         edition = \"2024\"\n\n[dependencies]\n\
         siderite = {{ path = \"{}\" }}\n",
        siderite_path()
    );
    fs::write(dir.join("Cargo.toml"), manifest).unwrap();
    fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    for bin in bins {
        let file = dir.join("src/bin").join(format!("{bin}.rs"));
        fs::write(file, "fn main() {}\n").unwrap();
    }
}

fn virtual_workspace(root: &Path, members: &[&str]) {
    let quoted = members.iter().map(|m| format!("\"{m}\""));
    let list: Vec<String> = quoted.collect();
    let text = format!(
        "[workspace]\nresolver = \"3\"\nmembers = [{}]\n",
        list.join(", ")
    );
    fs::write(root.join("Cargo.toml"), text).unwrap();
}

fn siderite(cwd: &Path, args: &[&str], path: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_siderite"));
    command.args(args).current_dir(cwd);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command.output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn virtual_workspace_with_two_apps_is_ambiguous() {
    let root = scratch("two");
    write_package(&root.join("crates/api"), "api", &[]);
    write_package(&root.join("crates/worker"), "worker", &[]);
    virtual_workspace(&root, &["crates/api", "crates/worker"]);
    let out = siderite(&root, &["routes"], None);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(text.contains("api ("), "{text}");
    assert!(text.contains("worker ("), "{text}");
    assert!(text.contains("--package"), "{text}");
    assert!(text.contains("siderite sel"), "{text}");
}

#[test]
fn unknown_package_and_manifest_are_usage_errors() {
    let root = scratch("flags");
    write_package(&root.join("crates/api"), "api", &[]);
    virtual_workspace(&root, &["crates/api"]);
    let out = siderite(&root, &["-p", "ghost", "routes"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("members: api"), "{}", stderr(&out));
    let args = ["--manifest-path", "none.toml", "check"];
    let out = siderite(&root, &args, None);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("no such file"), "{}", stderr(&out));
}

#[test]
fn several_binaries_need_a_choice() {
    let root = scratch("bins");
    let app = root.join("app");
    write_package(&app, "app", &["extra"]);
    let out = siderite(&app, &["routes"], None);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let text = stderr(&out);
    assert!(text.contains("app, extra"), "{text}");
    assert!(text.contains("--bin NAME"), "{text}");
    let out = siderite(&app, &["--bin", "nope", "routes"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("no binary `nope`"));
}

#[test]
fn virtual_workspace_without_an_app_keeps_the_old_message() {
    let root = scratch("none");
    fs::create_dir_all(root.join("lib/src")).unwrap();
    fs::write(root.join("lib/src/lib.rs"), "").unwrap();
    fs::write(
        root.join("lib/Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    virtual_workspace(&root, &["lib"]);
    let out = siderite(&root, &["routes"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("needs an application"));
}

#[test]
fn missing_cargo_is_a_clear_runtime_error() {
    let root = scratch("nocargo");
    write_package(&root.join("app"), "app", &[]);
    let empty = root.join("empty path");
    fs::create_dir_all(&empty).unwrap();
    let path = empty.display().to_string();
    let out = siderite(&root.join("app"), &["routes"], Some(&path));
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("not found on PATH"),
        "{}",
        stderr(&out)
    );
}
