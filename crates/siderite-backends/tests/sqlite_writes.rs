//! Concurrent writers on a pooled SQLite file queue for one write slot.
#![cfg(feature = "sqlite")]

use siderite_backends::sqlite::{GroupCommit, SqliteBackend};
use siderite_orm::{Backend, BackendError, Db, Executor, InsertPlan, OrmError, Value, WritePlan};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A fresh database file under the system temp directory, removed on drop.
struct TempDb(PathBuf);

impl TempDb {
    fn new() -> Self {
        let name = format!("siderite-writes-{}.db", uuid::Uuid::new_v4());
        Self(std::env::temp_dir().join(name))
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.0.display())
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let mut path = self.0.clone().into_os_string();
            path.push(suffix);
            drop(std::fs::remove_file(path));
        }
    }
}

async fn open(file: &TempDb) -> Result<SqliteBackend, OrmError> {
    let backend = SqliteBackend::connect(&file.url()).await?;
    backend
        .execute_script("CREATE TABLE items (id INTEGER PRIMARY KEY, n INTEGER NOT NULL UNIQUE)")
        .await?;
    Ok(backend)
}

async fn count(backend: &SqliteBackend) -> Result<Option<Value>, OrmError> {
    let rows = backend
        .fetch_raw("SELECT COUNT(*) AS n FROM items", Vec::new())
        .await?;
    Ok(rows.rows[0].get("n").cloned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pooled_concurrent_inserts_never_meet_the_busy_handler() -> Result<(), OrmError> {
    let file = TempDb::new();
    let db = Db::new(open(&file).await?);
    let started = Instant::now();
    let tasks: Vec<_> = (0..200)
        .map(|n| {
            let db = db.clone();
            tokio::spawn(async move {
                db.raw_execute("INSERT INTO items (n) VALUES (?)", vec![Value::Int(n)])
                    .await
            })
        })
        .collect();
    for task in tasks {
        assert_eq!(task.await.expect("insert task panicked")?, 1);
    }
    let rows = db
        .raw_sql("SELECT COUNT(*) AS n FROM items", Vec::new())
        .await?;
    assert_eq!(rows.rows[0].get("n"), Some(&Value::Int(200)));
    // Racing writers would wait out the five second busy timeout or fail.
    assert!(started.elapsed() < Duration::from_secs(30));
    Ok(())
}

#[tokio::test]
async fn write_outside_an_open_transaction_fails_instead_of_hanging() -> Result<(), OrmError> {
    let file = TempDb::new();
    let backend = open(&file)
        .await?
        .with_write_timeout(Duration::from_millis(200));
    let tx = backend.begin(None).await?;
    // Reads still use the rest of the pool.
    assert_eq!(count(&backend).await?, Some(Value::Int(0)));
    let started = Instant::now();
    let err = backend
        .execute_raw("INSERT INTO items (n) VALUES (1)", Vec::new())
        .await
        .expect_err("the transaction holds the write slot");
    assert!(err.to_string().contains("database is locked"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(2));
    tx.execute_raw("INSERT INTO items (n) VALUES (2)", Vec::new())
        .await?;
    tx.commit().await?;
    // Committing releases the slot.
    backend
        .execute_raw("INSERT INTO items (n) VALUES (3)", Vec::new())
        .await?;
    assert_eq!(count(&backend).await?, Some(Value::Int(2)));
    Ok(())
}

#[tokio::test]
async fn rollback_and_drop_release_the_write_slot() -> Result<(), OrmError> {
    let file = TempDb::new();
    let backend = open(&file)
        .await?
        .with_write_timeout(Duration::from_secs(2));
    let tx = backend.begin(None).await?;
    tx.execute_raw("INSERT INTO items (n) VALUES (1)", Vec::new())
        .await?;
    tx.rollback().await?;
    drop(backend.begin(None).await?);
    backend
        .execute_raw("INSERT INTO items (n) VALUES (2)", Vec::new())
        .await?;
    assert_eq!(count(&backend).await?, Some(Value::Int(1)));
    Ok(())
}

#[tokio::test]
async fn schema_changes_release_the_write_slot() -> Result<(), OrmError> {
    let file = TempDb::new();
    let backend = open(&file)
        .await?
        .with_write_timeout(Duration::from_secs(2));
    let schema = backend.begin_schema(true).await?;
    schema
        .execute_script("CREATE TABLE extra (id INTEGER PRIMARY KEY)")
        .await?;
    schema.commit().await?;
    // A non-transactional schema connection leaves pool writes open.
    let loose = backend.begin_schema(false).await?;
    backend
        .execute_raw("INSERT INTO items (n) VALUES (1)", Vec::new())
        .await?;
    loose.commit().await?;
    assert_eq!(count(&backend).await?, Some(Value::Int(1)));
    Ok(())
}

fn insert(n: i64) -> WritePlan {
    WritePlan::Insert(InsertPlan {
        table: "items".into(),
        columns: vec!["n".into()],
        rows: vec![vec![Value::Int(n)]],
        returning: vec!["id".into(), "n".into()],
    })
}

async fn grouped(file: &TempDb) -> Result<SqliteBackend, OrmError> {
    Ok(open(file).await?.group_commit(GroupCommit {
        max_batch: 16,
        queue: 8,
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn group_commit_isolates_failures_and_returns_each_row() -> Result<(), OrmError> {
    let file = TempDb::new();
    let backend = grouped(&file).await?;
    // 0..100 plus duplicates of 7 and 42: exactly two writes must fail.
    let values: Vec<i64> = (0..100).chain([7, 42]).collect();
    let tasks: Vec<_> = values
        .iter()
        .map(|&n| {
            let backend = backend.clone();
            tokio::spawn(async move { (n, backend.execute(&insert(n)).await) })
        })
        .collect();
    let mut failed = 0;
    let mut ids = std::collections::HashSet::new();
    for task in tasks {
        let (n, result) = task.await.expect("insert task panicked");
        match result {
            Ok(done) => {
                assert_eq!(done.rows_affected, 1);
                let row = &done.returning[0];
                assert_eq!(row.get("n"), Some(&Value::Int(n)));
                let Some(Value::Int(id)) = row.get("id") else {
                    panic!("missing id for {n}");
                };
                assert!(ids.insert(*id), "id {id} returned twice");
            }
            Err(OrmError::Backend(BackendError::Constraint(_))) => {
                assert!(n == 7 || n == 42, "unexpected failure for {n}");
                failed += 1;
            }
            Err(other) => panic!("unexpected error for {n}: {other}"),
        }
    }
    assert_eq!(failed, 2);
    assert_eq!(ids.len(), 100);
    assert_eq!(count(&backend).await?, Some(Value::Int(100)));
    // Every acknowledged row is visible to a separate connection.
    let observer = SqliteBackend::connect(&file.url()).await?;
    assert_eq!(count(&observer).await?, Some(Value::Int(100)));
    Ok(())
}

#[tokio::test]
async fn group_commit_runs_a_lone_write_and_leaves_raw_writes_alone() -> Result<(), OrmError> {
    let file = TempDb::new();
    let backend = grouped(&file).await?;
    let done = backend.execute(&insert(1)).await?;
    assert_eq!(done.returning[0].get("n"), Some(&Value::Int(1)));
    backend
        .execute_raw("INSERT INTO items (n) VALUES (2)", Vec::new())
        .await?;
    let tx = backend.begin(None).await?;
    tx.execute(&insert(3)).await?;
    tx.commit().await?;
    assert_eq!(count(&backend).await?, Some(Value::Int(3)));
    Ok(())
}

#[tokio::test]
async fn group_commit_waits_for_an_open_transaction() -> Result<(), OrmError> {
    let file = TempDb::new();
    let backend = open(&file)
        .await?
        .with_write_timeout(Duration::from_millis(200))
        .group_commit(GroupCommit::default());
    let tx = backend.begin(None).await?;
    let err = backend
        .execute(&insert(1))
        .await
        .expect_err("the transaction holds the write slot");
    assert!(err.to_string().contains("database is locked"), "{err}");
    tx.commit().await?;
    backend.execute(&insert(2)).await?;
    assert_eq!(count(&backend).await?, Some(Value::Int(1)));
    Ok(())
}
