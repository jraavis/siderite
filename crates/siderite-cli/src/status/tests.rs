//! `showmigrations --json` through [`AppCli`](crate::AppCli) on SQLite.

#![allow(clippy::unwrap_used)]

use super::*;
use crate::app_cli::Env;
use crate::fixtures::ScratchDir;
use crate::{AppCli, CliSettings};
use serde_json::Value;
use siderite_core::App;
use siderite_migrations::{Migration, Operation, write_migration};
use std::path::PathBuf;

const SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/schemas/cli-report-v1.json"
));

fn table(id: &str, deps: &[&str], name: &str) -> Migration {
    let op = Operation::RunSQL {
        sql: format!("CREATE TABLE {name} (id INTEGER PRIMARY KEY)"),
        reverse_sql: Some(format!("DROP TABLE {name}")),
    };
    let deps = deps.iter().map(|d| (*d).to_owned()).collect();
    Migration::new(id, deps, vec![op], true, vec![]).unwrap()
}

struct Project {
    scratch: ScratchDir,
    url: String,
}

impl Project {
    /// Two migration files; only the first applied.
    fn new(tag: &str) -> Self {
        let scratch = ScratchDir::new(tag);
        let url = format!(
            "sqlite://{}?mode=rwc",
            scratch.path().join("app.db").display()
        );
        let project = Self { scratch, url };
        write_migration(&project.dir(), &table("0001_a", &[], "a")).unwrap();
        assert_eq!(project.run(&["migrate"], None).0, 0);
        let second = table("0002_b", &["0001_a"], "b");
        write_migration(&project.dir(), &second).unwrap();
        project
    }

    fn dir(&self) -> PathBuf {
        self.scratch.path().join("migrations")
    }

    fn cli(&self) -> AppCli {
        AppCli::new(App::new)
            .settings(CliSettings::new().database("default", &self.url))
            .migrations_dir(self.dir())
    }

    /// Exit code and the report written to the channel, if any.
    fn run(&self, args: &[&str], cli: Option<AppCli>) -> (u8, Value) {
        let out = self.scratch.path().join("report.json");
        let _ = std::fs::remove_file(&out);
        let env = Env {
            report_path: Some(out.clone()),
            ..Env::default()
        };
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let cli = cli.unwrap_or_else(|| self.cli());
        let code = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(cli.execute(&args, &env))
            .unwrap_or_else(|err| err.exit_code());
        let text = std::fs::read_to_string(&out).unwrap_or_default();
        (code, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
}

fn assert_valid(value: &Value) {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator.iter_errors(value).collect();
    assert!(errors.is_empty(), "{errors:?}\n{value}");
}

#[test]
fn json_rows_match_the_text_rows() {
    let project = Project::new("status-rows");
    let (code, report) = project.run(&["showmigrations", "--json"], None);
    assert_eq!(code, 0);
    assert_valid(&report);
    let data = &report["data"];
    assert_eq!(data["alias"], "default");
    assert_eq!(data["source"], "settings");
    assert_eq!(
        (data["applied"].clone(), data["pending"].clone()),
        (1.into(), 1.into())
    );
    // The text renderer over the same rows prints the same order/marks.
    let rows: Vec<(String, bool)> = data["migrations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            let id = m["id"].as_str().unwrap().to_owned();
            (id, m["applied"].as_bool().unwrap())
        })
        .collect();
    let text = siderite_migrations::cli::render_status(&rows);
    assert_eq!(text, "[X] 0001_a\n[ ] 0002_b\n");
}

#[test]
fn flags_select_the_alias_and_source() {
    let project = Project::new("status-flags");
    let url = format!("--database-url={}", project.url);
    let args = ["--json", "showmigrations", url.as_str()];
    let (code, report) = project.run(&args, None);
    assert_eq!(code, 0);
    assert_eq!(report["data"]["source"], "flag");
    let replica = AppCli::new(App::new)
        .settings(CliSettings::new().database("replica", &project.url))
        .migrations_dir(project.dir());
    let args = ["showmigrations", "--json", "--database", "replica"];
    let (code, report) = project.run(&args, Some(replica));
    assert_eq!(code, 0);
    assert_eq!(report["data"]["alias"], "replica");
    assert_eq!(report["data"]["applied"], 1);
}

#[test]
fn failures_are_never_an_empty_success() {
    let project = Project::new("status-fail");
    // No database for the alias.
    let none = AppCli::new(App::new).migrations_dir(project.dir());
    let json = ["showmigrations", "--json"];
    let (code, report) = project.run(&json, Some(none));
    assert_eq!(code, 1);
    assert_valid(&report);
    assert_eq!(report["ok"], false);
    assert!(report["data"].is_null());
    // A changed, already applied file fails the checksum check.
    let changed = table("0001_a", &[], "changed");
    write_migration(&project.dir(), &changed).unwrap();
    let (code, report) = project.run(&["showmigrations", "--json"], None);
    assert_eq!(code, 1);
    let message = report["diagnostics"][0]["message"].as_str().unwrap();
    assert!(message.contains("checksum"), "{message}");
}

#[test]
fn connection_errors_are_redacted() {
    let project = Project::new("status-redact");
    // Rejected before any network I/O; the driver path is bounded below.
    let url = "ftp://root:hunter2@127.0.0.1:1/none";
    let flag = format!("--database-url={url}");
    let args = ["showmigrations", "--json", flag.as_str()];
    let (code, report) = project.run(&args, None);
    assert_eq!(code, 1);
    let text = report.to_string();
    assert!(!text.contains("hunter2"), "{text}");
    assert!(text.contains("`default`"), "{text}");
}

#[tokio::test]
async fn a_silent_server_hits_the_bound() {
    // Accepts connections but never answers the handshake.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let scratch = ScratchDir::new("status-bound");
    let selection = Selection {
        url: format!("postgres://u:p@127.0.0.1:{port}/x"),
        alias: Some("default".to_owned()),
        source: UrlSource::Flag,
    };
    let bound = Duration::from_millis(300);
    let started = std::time::Instant::now();
    let err = status(&selection, scratch.path(), bound).await.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        matches!(err, CliError::Timeout { .. } | CliError::Connect { .. }),
        "{err:?}"
    );
    drop(listener);
}

/// `url` with its password replaced by `hunter2wrong`.
fn wrong_password(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap();
    let (userinfo, host) = rest.rsplit_once('@').unwrap();
    let user = userinfo.split(':').next().unwrap();
    format!("{scheme}://{user}:hunter2wrong@{host}")
}

async fn live_status(var: &str) {
    let Ok(url) = std::env::var(var) else {
        panic!("{var} is required for this live test");
    };
    let scratch = ScratchDir::new("status-live");
    let selection = Selection {
        url: url.clone(),
        alias: Some("default".to_owned()),
        source: UrlSource::Environment,
    };
    // An empty migrations directory: a read-only status of nothing.
    let report = status(&selection, scratch.path(), STATUS_TIMEOUT)
        .await
        .unwrap();
    assert!(report.ok);
    assert!(report.data.migrations.is_empty());
    let selection = Selection {
        url: wrong_password(&url),
        ..selection
    };
    let started = std::time::Instant::now();
    let err = status(&selection, scratch.path(), STATUS_TIMEOUT)
        .await
        .unwrap_err();
    assert!(started.elapsed() < STATUS_TIMEOUT, "unbounded: {err}");
    let envelope = crate::report::channel::error_envelope("x", &err);
    let text = serde_json::to_string(&envelope).unwrap();
    assert!(!text.contains("hunter2wrong"), "{text}");
    assert!(text.contains("`default`"), "{text}");
}

#[tokio::test]
#[ignore = "live PostgreSQL; set DATABASE_URL"]
async fn live_postgres_status() {
    live_status("DATABASE_URL").await;
}

#[tokio::test]
#[ignore = "live MySQL; set MYSQL_URL"]
async fn live_mysql_status() {
    live_status("MYSQL_URL").await;
}
