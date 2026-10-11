//! Seed datasets: arguments, selection, transactions and reports.

#![allow(clippy::unwrap_used)]

use super::*;
use crate::AppCli;
use crate::app_cli::Env;
use crate::connect::connect_url;
use crate::fixtures::{ScratchDir, assert_schema_valid};
use siderite_core::App;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

#[test]
fn arguments() {
    let parsed = |words: &[&str]| parse(&args(words));
    assert_eq!(parsed(&["seed", "--list"]).unwrap(), Request::List);
    let load = Request::Load("demo".to_owned());
    assert_eq!(parsed(&["seed", "--dataset", "demo"]).unwrap(), load);
    assert_eq!(parsed(&["seed", "--dataset=demo"]).unwrap(), load);
    for bad in [
        &["seed"][..],
        &["seed", "--dataset"],
        &["seed", "--dataset="],
        &["seed", "--dataset", "--list"],
        &["seed", "--dataset", "a", "b"],
        &["seed", "--list", "x"],
        &["seed", "--reset"],
    ] {
        assert!(parsed(bad).is_err(), "{bad:?}");
    }
}

/// `rows` rows in `table`, then (for `broken`) a failure.
fn dataset(name: &str, table: &str, rows: u32, broken: bool) -> Dataset {
    let table = table.to_owned();
    Dataset::new(name, "notes", move |s: Seeding| {
        let table = table.clone();
        async move {
            for n in 0..rows {
                let insert = format!("INSERT INTO {table} (n) VALUES ({n})");
                s.db.raw_execute(&insert, Vec::new()).await?;
            }
            if broken {
                return Err("password=hunter2 rejected".into());
            }
            Ok(())
        }
    })
}

async fn count(db: &Db, table: &str) -> usize {
    let select = format!("SELECT n FROM {table}");
    let found = db.raw_sql(&select, Vec::new()).await;
    found.unwrap().rows.len()
}

fn sqlite(dir: &ScratchDir) -> String {
    format!("sqlite://{}?mode=rwc", dir.path().join("seed.db").display())
}

/// A dataset loads in one transaction, and a failing one is rolled back
/// and reported without its secret.
async fn rolls_back(url: &str, table: &str) {
    let db = connect_url(url).await.unwrap();
    let drop = format!("DROP TABLE IF EXISTS {table}");
    db.raw_execute(&drop, Vec::new()).await.unwrap();
    let create = format!("CREATE TABLE {table} (n BIGINT)");
    db.raw_execute(&create, Vec::new()).await.unwrap();
    let datasets = [
        dataset("demo", table, 3, false),
        dataset("bad", table, 2, true),
    ];
    let target = |db: &Db| Target {
        db: db.clone(),
        alias: "default".to_owned(),
        source: UrlSource::Flag,
        url: url.to_owned(),
    };
    let report = load(&datasets, &datasets[0], target(&db)).await;
    assert!(report.ok, "{:?}", report.diagnostics);
    assert_eq!(count(&db, table).await, 3);
    let loaded = report.data.loaded.as_ref().unwrap();
    assert!(loaded.transaction);
    assert_eq!(loaded.dataset, "demo");
    assert_eq!(loaded.database, "default");
    let json = serde_json::to_value(&report).unwrap();
    assert!(!json.to_string().contains("hunter2"));
    assert_schema_valid(&json);

    let failed = load(&datasets, &datasets[1], target(&db)).await;
    assert_eq!(count(&db, table).await, 3, "rolled back");
    assert_eq!(failed.diagnostics[0].code(), "seed.E001");
    let text = render(&failed);
    assert!(!text.contains("hunter2"), "{text}");
    assert!(text.contains("rolled back"), "{text}");
    db.raw_execute(&drop, Vec::new()).await.unwrap();
}

#[tokio::test]
async fn a_failing_dataset_leaves_nothing_behind() {
    let dir = ScratchDir::new("seed");
    rolls_back(&sqlite(&dir), "notes").await;
}

async fn live(variable: &str) {
    let url = std::env::var(variable).unwrap();
    let table = format!("seed_test_{}", std::process::id());
    rolls_back(&url, &table).await;
}

#[tokio::test]
#[ignore = "live PostgreSQL; set DATABASE_URL"]
async fn live_postgres_seed() {
    live("DATABASE_URL").await;
}

#[tokio::test]
#[ignore = "live MySQL; set MYSQL_URL"]
async fn live_mysql_seed() {
    live("MYSQL_URL").await;
}

#[test]
fn unknown_and_duplicate_names_are_usage_errors() {
    let datasets = [dataset("a", "t", 0, false), dataset("a", "t", 0, false)];
    let unknown = find(&datasets[..1], "b").unwrap_err().to_string();
    assert!(unknown.contains("registered: a"), "{unknown}");
    let twice = find(&datasets, "a").unwrap_err().to_string();
    assert!(twice.contains("more than once"), "{twice}");
    let none = find(&[], "a").unwrap_err().to_string();
    assert!(none.contains("none is registered"), "{none}");
}

#[tokio::test]
async fn the_command_lists_and_loads_through_the_app_cli() {
    let dir = ScratchDir::new("seed-cli");
    let url = sqlite(&dir);
    let db = connect_url(&url).await.unwrap();
    let create = "CREATE TABLE notes (n INTEGER)";
    db.raw_execute(create, Vec::new()).await.unwrap();
    let cli = AppCli::new(App::new).seed("demo", "Two notes", |s| async move {
        let insert = "INSERT INTO notes (n) VALUES (1), (2)";
        s.db.raw_execute(insert, Vec::new()).await?;
        Ok(())
    });
    let env = Env::default();
    let run = async |words: &[&str]| cli.execute(&args(words), &env).await;
    assert_eq!(run(&["seed", "--list"]).await.unwrap(), 0);
    let load = ["--database-url", &url, "seed", "--dataset", "demo"];
    assert_eq!(run(&load).await.unwrap(), 0);
    assert_eq!(count(&db, "notes").await, 2);
    let unknown = run(&["seed", "--dataset", "nope"]).await.unwrap_err();
    assert_eq!(unknown.exit_code(), 2);
    let none = run(&["seed", "--dataset", "demo"]).await.unwrap_err();
    assert!(matches!(none, CliError::NoDatabase { .. }), "{none}");
}
