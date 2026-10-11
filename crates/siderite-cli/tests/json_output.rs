//! `siderite --json` end to end, with a stand-in `cargo` on `PATH`.
//!
//! The stand-in plays the application: it prints noise on stdout (as an
//! app factory might), writes `$FAKE_REPORT` to the report file named by
//! `SIDERITE_REPORT_PATH` and exits with `$FAKE_CODE`. The tests check
//! that stdout carries exactly the report, or one failure envelope, and
//! that exit codes keep their meaning.

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FAKE_CARGO: &str = r#"#!/bin/sh
echo "noise from the app factory"
echo "cargo args: $*" >&2
if [ -n "$SIDERITE_REPORT_PATH" ] && [ -n "$FAKE_REPORT" ]; then
  printf '%s\n' "$FAKE_REPORT" > "$SIDERITE_REPORT_PATH"
fi
exit "${FAKE_CODE:-0}"
"#;

/// A temp directory removed on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("siderite json {id}-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

/// A plain app package and a `bin/cargo` stand-in under `root`.
fn fixture(root: &Path) -> (PathBuf, String) {
    let app = root.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\
                    edition = \"2024\"\n\n[dependencies]\n\
                    siderite = \"0.1\"\n";
    fs::write(app.join("Cargo.toml"), manifest).unwrap();
    fs::write(app.join("src/main.rs"), "fn main() {}\n").unwrap();
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let cargo = bin.join("cargo");
    fs::write(&cargo, FAKE_CARGO).unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    (app, format!("{}:{path}", bin.display()))
}

type Fake<'a> = &'a [(&'a str, &'a str)];

fn siderite(cwd: &Path, path: &str, args: &[&str], fake: Fake) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_siderite"));
    command.args(args).current_dir(cwd).env("PATH", path);
    command.env_remove("SIDERITE_REPORT_PATH");
    command.env_remove("DATABASE_URL");
    for (key, value) in fake {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Stdout must be exactly one JSON value and a newline.
fn one_value(out: &Output) -> Value {
    let text = stdout(out);
    assert!(text.ends_with('\n'), "{text:?}");
    assert_eq!(text.matches('\n').count(), 1, "{text:?}");
    serde_json::from_str(&text).unwrap()
}

const REPORT: &str = concat!(
    r#"{"schema_version":1,"command":"routes","ok":true,"#,
    r#""data":{"routes":[]},"diagnostics":[]}"#
);

#[test]
fn the_report_is_the_only_stdout_output() {
    let root = scratch("report");
    let (app, path) = fixture(&root.0);
    for args in [["--json", "routes"], ["routes", "--json"]] {
        let fake = [("FAKE_REPORT", REPORT)];
        let out = siderite(&app, &path, &args, &fake);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        assert_eq!(stdout(&out), format!("{REPORT}\n"));
        let err = stderr(&out);
        assert!(err.contains("noise from the app factory"), "{err}");
        // Arguments reach the app unchanged, after `run --`.
        assert!(err.contains("cargo args: run --"), "{err}");
        assert!(err.contains("--json"), "{err}");
    }
}

#[test]
fn the_app_exit_code_is_kept_with_its_report() {
    let root = scratch("failing");
    let (app, path) = fixture(&root.0);
    let report = REPORT.replace("\"ok\":true", "\"ok\":false");
    let fake = [("FAKE_REPORT", report.as_str()), ("FAKE_CODE", "1")];
    let out = siderite(&app, &path, &["check", "--json"], &fake);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(one_value(&out)["ok"], false);
}

#[test]
fn a_missing_or_invalid_report_becomes_a_failure_envelope() {
    let root = scratch("missing");
    let (app, path) = fixture(&root.0);
    let cases = [
        ("", "101", 1),
        ("", "2", 2),
        ("not json", "0", 1),
        ("{} {}", "0", 1),
    ];
    for (report, code, want) in cases {
        let fake = [("FAKE_REPORT", report), ("FAKE_CODE", code)];
        let out = siderite(&app, &path, &["check", "--json"], &fake);
        assert_eq!(out.status.code(), Some(want), "{report:?} {code}");
        let value = one_value(&out);
        assert_eq!(value["command"], "check");
        assert_eq!(value["ok"], false);
        assert!(value["data"].is_null());
        assert_eq!(value["diagnostics"][0]["code"], "cli.E003");
    }
}

#[test]
fn usage_errors_of_the_tool_are_envelopes() {
    let root = scratch("usage");
    let (app, path) = fixture(&root.0);
    let empty = root.0.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let cases: [(&Path, &[&str], &str); 4] = [
        (&empty, &["--json"], "siderite"),
        (&empty, &["check", "--json"], "check"),
        (&app, &["--json", "run"], "run"),
        (&app, &["routes", "--json", "--help"], "routes"),
    ];
    for (cwd, args, command) in cases {
        let out = siderite(cwd, &path, args, &[("FAKE_REPORT", REPORT)]);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let value = one_value(&out);
        assert_eq!(value["command"], command, "{args:?}");
        assert_eq!(value["diagnostics"][0]["code"], "cli.E001");
        assert!(!stderr(&out).is_empty());
    }
}

#[test]
fn openapi_export_prints_only_the_document() {
    let root = scratch("openapi");
    let (app, path) = fixture(&root.0);
    let doc = r#"{"openapi":"3.1.0"}"#;
    let export = ["openapi", "export"];
    let out = siderite(&app, &path, &export, &[("FAKE_REPORT", doc)]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), format!("{doc}\n"));
    assert!(stderr(&out).contains("noise from the app factory"));
    // No document: nothing on stdout (no envelope), the reason on stderr.
    let fake = [("FAKE_REPORT", ""), ("FAKE_CODE", "101")];
    let out = siderite(&app, &path, &["openapi", "export"], &fake);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("without writing its output"));
}

#[test]
fn openapi_output_paths_are_relative_to_the_caller() {
    let root = scratch("openapi-output");
    let (app, path) = fixture(&root.0);
    let cwd = app.join("src");
    let args = ["openapi", "export", "-o", "spec.json"];
    let out = siderite(&cwd, &path, &args, &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let want = format!("-o {}", cwd.join("spec.json").display());
    assert!(stderr(&out).contains(&want), "{}", stderr(&out));
}

#[test]
fn standalone_status_agrees_with_the_text() {
    use siderite_migrations::{Migration, Operation, write_migration};
    let root = scratch("standalone");
    let dir = root.0.join("migrations");
    for (id, deps, table) in [
        ("0001_a", vec![], "a"),
        ("0002_b", vec!["0001_a".to_owned()], "b"),
    ] {
        let op = Operation::RunSQL {
            sql: format!("CREATE TABLE {table} (id INTEGER)"),
            reverse_sql: Some(format!("DROP TABLE {table}")),
        };
        let m = Migration::new(id, deps, vec![op], true, vec![]).unwrap();
        write_migration(&dir, &m).unwrap();
    }
    let db = root.0.join("app.db");
    let url = format!("sqlite://{}?mode=rwc", db.display());
    let path = std::env::var("PATH").unwrap_or_default();
    let flag = format!("--database-url={url}");
    let out = siderite(&root.0, &path, &["migrate", "0001_a", &flag], &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = siderite(&root.0, &path, &["showmigrations", &flag], &[]);
    assert_eq!(stdout(&text), "[X] 0001_a\n[ ] 0002_b\n");
    let json = ["showmigrations", "--json", &flag];
    let out = siderite(&root.0, &path, &json, &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let value = one_value(&out);
    assert!(value["data"]["alias"].is_null());
    assert_eq!(value["data"]["source"], "flag");
    let rows: Vec<String> = value["data"]["migrations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            let mark = if m["applied"] == true { 'X' } else { ' ' };
            format!("[{mark}] {}\n", m["id"].as_str().unwrap())
        })
        .collect();
    assert_eq!(rows.concat(), stdout(&text));
    // DATABASE_URL is reported as the environment, never echoed.
    let env = [("DATABASE_URL", url.as_str())];
    let out = siderite(&root.0, &path, &["--json", "showmigrations"], &env);
    assert_eq!(one_value(&out)["data"]["source"], "environment");
    assert!(!stdout(&out).contains("app.db"));
    // No database at all is a failure envelope, not an empty status.
    let out = siderite(&root.0, &path, &["showmigrations", "--json"], &[]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(one_value(&out)["ok"], false);
}
