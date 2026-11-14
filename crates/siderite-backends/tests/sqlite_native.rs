//! The native SQLite backend: parity with the SQLx backend on the same SQL,
//! and the behaviour its connection layout promises (concurrent WAL reads,
//! queued writers, rollback on drop, the single in-memory connection, where
//! statements run, and single-process mode). The connection-layout tests
//! run in both locking modes.
#![cfg(feature = "sqlite-native")]

use chrono::{NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;
use siderite_backends::sqlite::SqliteBackend;
use siderite_backends::sqlite::native::{
    Dispatch, JournalMode, NativeSqliteBackend, NativeSqliteOptions,
};
use siderite_orm::{
    Backend, BackendError, Executor, InsertPlan, OrmError, QueryError, QueryResult, Value,
    WritePlan,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

type Check = Result<(), OrmError>;

/// A fresh database file under the system temp directory, removed on drop.
struct TempDb(PathBuf);

impl TempDb {
    fn new() -> Self {
        let name = format!("siderite-native-{}.db", uuid::Uuid::new_v4());
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

fn wal() -> NativeSqliteOptions {
    NativeSqliteOptions {
        journal_mode: Some(JournalMode::Wal),
        ..NativeSqliteOptions::default()
    }
}

fn single_process() -> NativeSqliteOptions {
    NativeSqliteOptions {
        single_process: true,
        ..wal()
    }
}

/// WAL options in both locking modes, for behaviour that must not depend
/// on the mode.
fn modes() -> [NativeSqliteOptions; 2] {
    [wal(), single_process()]
}

async fn both(schema: &str) -> Result<(SqliteBackend, NativeSqliteBackend), OrmError> {
    let sqlx = SqliteBackend::connect("sqlite::memory:").await?;
    let native = NativeSqliteBackend::connect("sqlite::memory:").await?;
    sqlx.execute_script(schema).await?;
    native.execute_script(schema).await?;
    Ok((sqlx, native))
}

async fn count(db: &(impl Executor + ?Sized), table: &str) -> Result<Value, OrmError> {
    let sql = format!("SELECT COUNT(*) AS n FROM {table}");
    let rows = db.fetch_raw(&sql, Vec::new()).await?.rows;
    Ok(rows[0].get("n").cloned().unwrap_or(Value::Null))
}

// ---- parity with the SQLx backend ----

const TYPES: &str = "CREATE TABLE cells (
    id INTEGER PRIMARY KEY, i INTEGER, r REAL, t TEXT, b BLOB, n NUMERIC, flag BOOLEAN)";

/// Every storage class, and every value kind as bound, reads back the same.
#[tokio::test]
async fn values_bind_and_decode_like_sqlx() -> Check {
    let (sqlx, native) = both(TYPES).await?;
    let rows: Vec<Vec<Value>> = vec![
        vec![
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
        ],
        vec![
            Value::Int(i64::MIN),
            Value::Float(-0.5),
            Value::Text("héllo".into()),
            Value::Bytes(vec![0, 159, 255]),
            Value::Decimal(Decimal::new(1250, 2)),
            Value::Bool(true),
        ],
        vec![
            Value::Int(i64::MAX),
            Value::Float(1e300),
            Value::Uuid(uuid::Uuid::from_u128(7)),
            Value::Bytes(Vec::new()),
            Value::Date(NaiveDate::from_ymd_opt(2026, 11, 13).unwrap()),
            Value::Bool(false),
        ],
        vec![
            Value::Text("12".into()),
            Value::Int(3),
            Value::Timestamp(Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()),
            Value::Text("text in a blob column".into()),
            Value::Json(serde_json::json!({"a": [1, 2]})),
            Value::Float(2.5),
        ],
    ];
    let sql = "INSERT INTO cells (i, r, t, b, n, flag) VALUES (?, ?, ?, ?, ?, ?)";
    for row in rows {
        assert_eq!(
            sqlx.execute_raw(sql, row.clone()).await?,
            native.execute_raw(sql, row).await?
        );
    }
    let select = "SELECT *, i + r AS mixed, typeof(n) AS kind, length(b) AS size FROM cells";
    let expected = sqlx.fetch_raw(select, Vec::new()).await?;
    assert_eq!(native.fetch_raw(select, Vec::new()).await?, expected);
    assert_eq!(expected.rows.len(), 4);
    // An empty result is empty on both.
    let none = "SELECT * FROM cells WHERE id < 0";
    assert_eq!(
        native.fetch_raw(none, Vec::new()).await?,
        QueryResult::default()
    );
    Ok(())
}

/// The same SQL answers the same on both backends: several statements,
/// numbered and named placeholders, missing and surplus parameters,
/// `RETURNING`, and change counts after a read.
#[tokio::test]
async fn statement_semantics_match_sqlx() -> Check {
    let schema = "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)";
    let (sqlx, native) = both(schema).await?;
    let writes: [(&str, Vec<Value>); 4] = [
        (
            "INSERT INTO t (v) VALUES (?); INSERT INTO t (v) VALUES (?), (?1)",
            vec![Value::Int(1), Value::Int(2)],
        ),
        (
            "UPDATE t SET v = $1 WHERE id > ?2",
            vec![Value::Int(9), Value::Int(1)],
        ),
        ("SELECT * FROM t", Vec::new()),
        ("INSERT INTO t (v) VALUES (?)", Vec::new()),
    ];
    for (sql, params) in writes {
        let expected = sqlx.execute_raw(sql, params.clone()).await;
        let got = native.execute_raw(sql, params).await;
        assert_eq!(
            got.map_err(|e| e.to_string()),
            expected.map_err(|e| e.to_string()),
            "{sql}"
        );
    }
    let reads: [(&str, Vec<Value>); 4] = [
        (
            "SELECT v FROM t ORDER BY id; SELECT ? AS v",
            vec![Value::Int(5)],
        ),
        ("SELECT ? AS a, ? AS b", vec![Value::Int(1)]),
        (
            "SELECT ?2 AS a",
            vec![Value::Int(1), Value::Int(2), Value::Int(3)],
        ),
        (
            "UPDATE t SET v = v + 1 WHERE id = 1 RETURNING id, v",
            Vec::new(),
        ),
    ];
    for (sql, params) in reads {
        let expected = sqlx.fetch_raw(sql, params.clone()).await?;
        assert_eq!(native.fetch_raw(sql, params).await?, expected, "{sql}");
    }
    let plan = WritePlan::Insert(InsertPlan {
        table: "t".into(),
        columns: vec!["v".into()],
        rows: vec![vec![Value::Int(7)], vec![Value::Int(8)]],
        returning: vec!["id".into(), "v".into()],
    });
    assert_eq!(native.execute(&plan).await?, sqlx.execute(&plan).await?);
    Ok(())
}

/// Failures classify into the same [`BackendError`] variant with the same
/// message.
#[tokio::test]
async fn errors_classify_like_sqlx() -> Check {
    let schema = "CREATE TABLE p (id INTEGER PRIMARY KEY);
        CREATE TABLE c (id INTEGER PRIMARY KEY, p INTEGER REFERENCES p(id),
            u TEXT UNIQUE, n INTEGER NOT NULL DEFAULT 0, k INTEGER CHECK (k > 0));
        INSERT INTO p VALUES (1); INSERT INTO c (id, p, u, k) VALUES (1, 1, 'x', 1);";
    let (sqlx, native) = both(schema).await?;
    let failing = [
        "INSERT INTO c (id) VALUES (1)",
        "INSERT INTO c (u) VALUES ('x')",
        "INSERT INTO c (p) VALUES (2)",
        "INSERT INTO c (k) VALUES (0)",
        "INSERT INTO c (n) VALUES (NULL)",
        "DELETE FROM p",
        "SELEC 1",
        "SELECT * FROM missing",
        "SELECT :named",
        "INSERT INTO c (id) VALUES (1) ON CONFLICT DO NOTHING; SELECT * FROM missing",
    ];
    for sql in failing {
        let expected = sqlx.execute_raw(sql, Vec::new()).await.unwrap_err();
        let got = native.execute_raw(sql, Vec::new()).await.unwrap_err();
        // SQLx tags its own protocol errors with a source location.
        let message = expected.to_string();
        let message = match message.split_once(" (sqlx_sqlite::") {
            Some((message, _)) => message.to_owned(),
            None => message,
        };
        assert_eq!(got.to_string(), message, "{sql}");
        let variant = |e: &OrmError| match e {
            OrmError::Backend(BackendError::Constraint(_)) => "constraint",
            OrmError::Backend(BackendError::Database(_)) => "database",
            _ => "other",
        };
        assert_eq!(variant(&got), variant(&expected), "{sql}");
    }
    Ok(())
}

/// A schema change that adds a foreign-key violation fails on both with the
/// same message, ignores violations that were already there, and leaves
/// enforcement on afterwards.
#[tokio::test]
async fn schema_transactions_check_new_violations_like_sqlx() -> Check {
    let schema = "CREATE TABLE p (id INTEGER PRIMARY KEY);
        CREATE TABLE c (id INTEGER PRIMARY KEY, p INTEGER REFERENCES p(id));
        PRAGMA foreign_keys = OFF; INSERT INTO c VALUES (1, 404); PRAGMA foreign_keys = ON;";
    let (sqlx, native) = both(schema).await?;
    let backends: [&dyn Backend; 2] = [&sqlx, &native];
    let mut messages = Vec::new();
    for db in backends {
        let tx = db.begin_schema(true).await?;
        tx.execute_script("INSERT INTO c VALUES (2, 405)").await?;
        messages.push(tx.commit().await.unwrap_err().to_string());
        assert_eq!(count(db, "c").await?, Value::Int(1));
        // The pre-existing violation alone does not block a migration.
        let tx = db.begin_schema(true).await?;
        tx.execute_script("CREATE TABLE extra (id INTEGER)").await?;
        tx.commit().await?;
        let fk = db.fetch_raw("PRAGMA foreign_keys", Vec::new()).await?;
        assert_eq!(
            fk.rows[0].column_at(0).map(|(_, v)| v),
            Some(&Value::Int(1))
        );
    }
    assert!(
        messages[0].contains("c row 2 (missing parent p)"),
        "{}",
        messages[0]
    );
    assert_eq!(messages[0], messages[1]);
    Ok(())
}

// ---- connection layout ----

/// With WAL, many reads run at once while a write transaction is open, and
/// see the last committed state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wal_readers_run_while_a_write_transaction_is_open() -> Check {
    for base in modes() {
        readers_run_beside_a_write_transaction(base).await?;
    }
    Ok(())
}

async fn readers_run_beside_a_write_transaction(base: NativeSqliteOptions) -> Check {
    let file = TempDb::new();
    let options = NativeSqliteOptions { readers: 4, ..base };
    let db = NativeSqliteBackend::connect_with(&file.url(), options).await?;
    db.execute_script("CREATE TABLE items (n INTEGER); INSERT INTO items VALUES (1)")
        .await?;
    let mode = db.fetch_raw("PRAGMA journal_mode", Vec::new()).await?;
    assert_eq!(
        mode.rows[0].get("journal_mode"),
        Some(&Value::Text("wal".into()))
    );
    let tx = db.begin(None).await?;
    tx.execute_raw("INSERT INTO items VALUES (2)", Vec::new())
        .await?;
    let reads: Vec<_> = (0..64)
        .map(|_| {
            let db = db.clone();
            tokio::spawn(async move { count(&db, "items").await })
        })
        .collect();
    let all = futures_util::future::join_all(reads);
    let counts = tokio::time::timeout(Duration::from_secs(5), all)
        .await
        .map_err(|_| BackendError::Database("reads waited for the writer".into()))?;
    for read in counts {
        let read = read.map_err(|e| BackendError::Database(e.to_string()))?;
        assert_eq!(read?, Value::Int(1));
    }
    tx.commit().await?;
    assert_eq!(count(&db, "items").await?, Value::Int(2));
    Ok(())
}

/// Every locking mode with every dispatch.
fn modes_and_dispatch() -> impl Iterator<Item = (NativeSqliteOptions, Dispatch)> {
    modes().into_iter().flat_map(|base| {
        [Dispatch::Inline, Dispatch::Blocking].map(|dispatch| (base.clone(), dispatch))
    })
}

/// Writers queue for the one writer connection instead of meeting SQLite's
/// busy handler: with no busy timeout at all, any lock contention would
/// fail at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writers_queue_instead_of_failing_busy() -> Check {
    for (base, writes) in modes_and_dispatch() {
        let file = TempDb::new();
        let options = NativeSqliteOptions {
            busy_timeout: Duration::ZERO,
            writes,
            ..base
        };
        let db = NativeSqliteBackend::connect_with(&file.url(), options).await?;
        db.execute_script("DROP TABLE IF EXISTS items; CREATE TABLE items (n INTEGER UNIQUE)")
            .await?;
        let tasks: Vec<_> = (0..200)
            .map(|n| {
                let db = db.clone();
                tokio::spawn(async move {
                    if n % 10 == 0 {
                        let tx = db.begin(None).await?;
                        tx.execute_raw("INSERT INTO items VALUES (?)", vec![Value::Int(n)])
                            .await?;
                        tx.commit().await.map(|()| 1)
                    } else {
                        db.execute_raw("INSERT INTO items VALUES (?)", vec![Value::Int(n)])
                            .await
                    }
                })
            })
            .collect();
        for task in tasks {
            assert_eq!(task.await.unwrap()?, 1);
        }
        assert_eq!(count(&db, "items").await?, Value::Int(200));
    }
    Ok(())
}

/// A transaction dropped without commit rolls back and frees the writer,
/// whichever way writes are dispatched.
#[tokio::test]
async fn dropped_transactions_roll_back() -> Check {
    for (base, writes) in modes_and_dispatch() {
        let file = TempDb::new();
        let options = NativeSqliteOptions { writes, ..base };
        let db = NativeSqliteBackend::connect_with(&file.url(), options)
            .await?
            .with_write_timeout(Duration::from_secs(2));
        db.execute_script("CREATE TABLE items (n INTEGER)").await?;
        let tx = db.begin(None).await?;
        tx.execute_raw("INSERT INTO items VALUES (1)", Vec::new())
            .await?;
        drop(tx);
        let schema = db.begin_schema(true).await?;
        schema
            .execute_script("PRAGMA foreign_keys = ON; INSERT INTO items VALUES (2)")
            .await?;
        drop(schema);
        db.execute_raw("INSERT INTO items VALUES (3)", Vec::new())
            .await?;
        let rows = db.fetch_raw("SELECT n FROM items", Vec::new()).await?;
        assert_eq!(rows.rows.len(), 1, "{writes:?}");
        assert_eq!(rows.rows[0].get("n"), Some(&Value::Int(3)));
        // The dropped schema transaction restored foreign keys.
        let tx = db.begin(None).await?;
        let fk = tx.fetch_raw("PRAGMA foreign_keys", Vec::new()).await?;
        assert_eq!(
            fk.rows[0].column_at(0).map(|(_, v)| v),
            Some(&Value::Int(1))
        );
        tx.rollback().await?;
    }
    Ok(())
}

/// A statement cancelled while it runs on the blocking pool takes the
/// transaction with it: it is rolled back, later calls fail as aborted, and
/// the writer is free again.
#[tokio::test]
async fn cancelled_offloaded_statement_aborts_its_transaction() -> Check {
    let db = NativeSqliteBackend::connect("sqlite::memory:").await?;
    db.execute_script("CREATE TABLE items (n INTEGER)").await?;
    let tx = db.begin(None).await?;
    tx.execute_raw("INSERT INTO items VALUES (1)", Vec::new())
        .await?;
    let slow = tx.execute_raw(SLOW_WRITE, Vec::new());
    assert!(
        tokio::time::timeout(Duration::from_millis(1), slow)
            .await
            .is_err()
    );
    assert!(matches!(
        tx.commit().await,
        Err(OrmError::Query(QueryError::TransactionAborted))
    ));
    assert_eq!(count(&db, "items").await?, Value::Int(0));
    db.execute_raw("INSERT INTO items VALUES (2)", Vec::new())
        .await?;
    assert_eq!(count(&db, "items").await?, Value::Int(1));
    Ok(())
}

/// An in-memory database is one connection: what a write leaves is what
/// every read sees, and a read waits while a transaction holds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_memory_database_is_one_connection() -> Check {
    let db = NativeSqliteBackend::connect("sqlite::memory:").await?;
    db.execute_script("CREATE TABLE items (n INTEGER)").await?;
    db.execute_raw("INSERT INTO items VALUES (1)", Vec::new())
        .await?;
    // A temporary table exists on its own connection only.
    db.execute_script("CREATE TEMP TABLE scratch (n INTEGER)")
        .await?;
    for _ in 0..20 {
        assert_eq!(count(&db, "scratch").await?, Value::Int(0));
    }
    let tx = db.begin(None).await?;
    tx.execute_raw("INSERT INTO items VALUES (2)", Vec::new())
        .await?;
    let read = tokio::spawn({
        let db = db.clone();
        async move { count(&db, "items").await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!read.is_finished(), "a read ran beside the transaction");
    tx.commit().await?;
    assert_eq!(read.await.unwrap()?, Value::Int(2));
    Ok(())
}

/// One million rows: long enough to see whether the runtime kept running.
const SLOW_READ: &str = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c \
    WHERE x < 1000000) SELECT COUNT(*) AS n FROM c";
const SLOW_WRITE: &str = "INSERT INTO items WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL \
    SELECT x + 1 FROM c WHERE x < 1000000) SELECT x FROM c";

/// How many times a 1 ms ticker on the same current-thread runtime fired
/// while `work` ran.
async fn ticks_during<F: Future<Output = Result<(), OrmError>>>(
    work: F,
) -> Result<usize, OrmError> {
    let ticks = Arc::new(AtomicUsize::new(0));
    let ticker = tokio::spawn({
        let ticks = Arc::clone(&ticks);
        async move {
            loop {
                tokio::time::sleep(Duration::from_millis(1)).await;
                ticks.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(5)).await;
    let before = ticks.load(Ordering::SeqCst);
    work.await?;
    let during = ticks.load(Ordering::SeqCst) - before;
    ticker.abort();
    Ok(during)
}

/// Inline statements run on the calling task, so nothing else on a
/// current-thread runtime runs meanwhile; offloaded ones leave it free.
#[tokio::test(flavor = "current_thread")]
async fn inline_blocks_the_runtime_and_blocking_does_not() -> Check {
    for (base, mode) in modes_and_dispatch() {
        let file = TempDb::new();
        let options = NativeSqliteOptions {
            reads: mode,
            writes: mode,
            ..base
        };
        let db = NativeSqliteBackend::connect_with(&file.url(), options).await?;
        db.execute_script("DROP TABLE IF EXISTS items; CREATE TABLE items (n INTEGER)")
            .await?;
        let read = ticks_during(async {
            let rows = db.fetch_raw(SLOW_READ, Vec::new()).await?;
            assert_eq!(rows.rows[0].get("n"), Some(&Value::Int(1_000_000)));
            Ok(())
        })
        .await?;
        let write = ticks_during(async {
            let changed = db.execute_raw(SLOW_WRITE, Vec::new()).await?;
            assert_eq!(changed, 1_000_000);
            Ok(())
        })
        .await?;
        match mode {
            Dispatch::Inline => assert_eq!((read, write), (0, 0)),
            Dispatch::Blocking => assert!(read > 0 && write > 0, "{read} {write}"),
        }
    }
    Ok(())
}

// ---- single-process mode ----

/// Readers see each commit as soon as it is made, whether it came from a
/// single statement or a transaction, and no `-shm` file appears: the WAL
/// index is in memory.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_process_readers_see_every_commit() -> Check {
    let file = TempDb::new();
    let options = NativeSqliteOptions {
        readers: 4,
        ..single_process()
    };
    let db = NativeSqliteBackend::connect_with(&file.url(), options).await?;
    db.execute_script("CREATE TABLE items (n INTEGER)").await?;
    for n in 1..=40 {
        if n % 4 == 0 {
            let tx = db.begin(None).await?;
            tx.execute_raw("INSERT INTO items VALUES (?)", vec![Value::Int(n)])
                .await?;
            tx.commit().await?;
        } else {
            db.execute_raw("INSERT INTO items VALUES (?)", vec![Value::Int(n)])
                .await?;
        }
        let reads: Vec<_> = (0..8)
            .map(|_| {
                let db = db.clone();
                tokio::spawn(async move { count(&db, "items").await })
            })
            .collect();
        for read in reads {
            assert_eq!(read.await.unwrap()?, Value::Int(n));
        }
    }
    assert!(file.with_suffix("-wal").exists());
    assert!(!file.with_suffix("-shm").exists());
    Ok(())
}

/// Another process is locked out while the backend has the database open:
/// a plain SQLite read fails with "database is locked" once its busy
/// timeout runs out, and so does a second single-process backend. Once the
/// backend is dropped the other process gets in.
#[tokio::test]
async fn single_process_locks_other_processes_out() -> Check {
    let file = TempDb::new();
    let db = NativeSqliteBackend::connect_with(&file.url(), single_process()).await?;
    db.execute_script("CREATE TABLE items (n INTEGER); INSERT INTO items VALUES (1)")
        .await?;
    assert_eq!(probe(&file, "read", "shared")?, "err database is locked");
    assert_eq!(
        probe(&file, "connect", "single")?,
        "err database error: error returned from database: (code: 5) database is locked"
    );
    // Inside the process, a connection of its own still works.
    assert_eq!(probe_in_process(&file), Ok(1));
    drop(db);
    assert_eq!(probe(&file, "read", "shared")?, "ok 1");
    Ok(())
}

/// The other way round: connecting fails, after the busy timeout, while
/// another process is using the database.
#[tokio::test]
async fn single_process_connect_fails_while_another_process_reads() -> Check {
    use std::io::{BufRead, BufReader};
    let file = TempDb::new();
    let db = NativeSqliteBackend::connect_with(&file.url(), wal()).await?;
    db.execute_script("CREATE TABLE items (n INTEGER)").await?;
    drop(db);
    let mut child = probe_command(&file, "hold", "shared")?
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| BackendError::Connection(e.to_string()))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines().map_while(Result::ok);
    let mut next_report = || lines.find_map(|line| probe_report(&line).map(str::to_owned));
    assert_eq!(next_report().as_deref(), Some("ready"));
    let options = NativeSqliteOptions {
        busy_timeout: Duration::from_millis(200),
        ..single_process()
    };
    let refused = NativeSqliteBackend::connect_with(&file.url(), options.clone()).await;
    assert_eq!(
        refused.map(drop).map_err(|e| e.to_string()),
        Err(
            "database connection error: database error: error returned from database: \
             (code: 5) database is locked"
                .into()
        )
    );
    drop(child.stdin.take());
    assert_eq!(next_report().as_deref(), Some("ok 0"));
    assert!(child.wait().is_ok_and(|status| status.success()));
    let db = NativeSqliteBackend::connect_with(&file.url(), options).await?;
    assert_eq!(count(&db, "items").await?, Value::Int(0));
    Ok(())
}

/// A process that dies without closing its connections leaves an
/// unchecked-pointed WAL behind; the next one to open the database finds
/// every committed transaction, in either mode.
#[tokio::test]
async fn reopening_after_a_crash_recovers_committed_data() -> Check {
    for (base, mode) in modes().into_iter().zip(["shared", "single"]) {
        let file = TempDb::new();
        assert_eq!(probe(&file, "crash", mode)?, "committed", "{mode}");
        let wal = std::fs::metadata(file.with_suffix("-wal")).map(|m| m.len());
        assert!(wal.as_ref().is_ok_and(|len| *len > 0), "{mode}: {wal:?}");
        let db = NativeSqliteBackend::connect_with(&file.url(), base).await?;
        assert_eq!(count(&db, "items").await?, Value::Int(100), "{mode}");
        let check = db.fetch_raw("PRAGMA integrity_check", Vec::new()).await?;
        assert_eq!(
            check.rows[0].column_at(0).map(|(_, v)| v),
            Some(&Value::Text("ok".into()))
        );
        db.execute_raw("INSERT INTO items VALUES (101)", Vec::new())
            .await?;
        assert_eq!(count(&db, "items").await?, Value::Int(101), "{mode}");
    }
    Ok(())
}

impl TempDb {
    fn with_suffix(&self, suffix: &str) -> PathBuf {
        let mut path = self.0.clone().into_os_string();
        path.push(suffix);
        PathBuf::from(path)
    }
}

/// Environment for [`child_process_probe`]: the database, what to do, and
/// the locking mode (`shared` or `single`).
const PROBE_DB: &str = "SIDERITE_NATIVE_PROBE_DB";
const PROBE_ACTION: &str = "SIDERITE_NATIVE_PROBE_ACTION";
const PROBE_MODE: &str = "SIDERITE_NATIVE_PROBE_MODE";

/// This test binary, rerun to execute only [`child_process_probe`].
fn probe_command(
    file: &TempDb,
    action: &str,
    mode: &str,
) -> Result<std::process::Command, OrmError> {
    let exe = std::env::current_exe().map_err(|e| BackendError::Connection(e.to_string()))?;
    let mut command = std::process::Command::new(exe);
    command
        .args(["child_process_probe", "--exact", "--nocapture"])
        .args(["--test-threads", "1"])
        .env(PROBE_DB, &file.0)
        .env(PROBE_ACTION, action)
        .env(PROBE_MODE, mode);
    Ok(command)
}

/// Run the probe in a second process and return what it reported.
fn probe(file: &TempDb, action: &str, mode: &str) -> Result<String, OrmError> {
    let output = probe_command(file, action, mode)?
        .output()
        .map_err(|e| BackendError::Connection(e.to_string()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report = stdout.lines().find_map(probe_report).map(str::to_owned);
    Ok(report.unwrap_or_else(|| {
        format!(
            "no report ({}): {stdout}{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
    }))
}

/// What the probe reported on `line`; libtest may print its own text first
/// on the same line.
fn probe_report(line: &str) -> Option<&str> {
    line.split_once("PROBE ").map(|(_, report)| report)
}

/// A plain SQLite connection in this process, outside the backend.
fn probe_in_process(file: &TempDb) -> Result<i64, String> {
    let conn = rusqlite::Connection::open(&file.0).map_err(|e| e.to_string())?;
    conn.query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
        .map_err(|e| e.to_string())
}

/// Not a test of its own: the second process the tests above start. Does
/// nothing unless [`PROBE_DB`] is set.
///
/// - `read`: a plain SQLite connection, as a `sqlite3` shell would open it,
///   with no busy timeout, counts `items`.
/// - `connect`: a single-process backend with a 100 ms busy timeout.
/// - `hold`: a plain connection reads, reports `ready`, and keeps the
///   database open until stdin closes.
/// - `crash`: a backend in the given mode commits 100 rows and aborts
///   without closing anything.
#[tokio::test]
async fn child_process_probe() -> Check {
    let Some(path) = std::env::var_os(PROBE_DB).map(PathBuf::from) else {
        return Ok(());
    };
    let action = std::env::var(PROBE_ACTION).unwrap_or_default();
    let base = match std::env::var(PROBE_MODE).as_deref() {
        Ok("single") => single_process(),
        _ => wal(),
    };
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let report = match action.as_str() {
        "read" => {
            let conn = rusqlite::Connection::open(&path).expect("open");
            conn.busy_timeout(Duration::ZERO).expect("busy timeout");
            match conn.query_row("SELECT COUNT(*) FROM items", [], |row| row.get::<_, i64>(0)) {
                Ok(n) => format!("ok {n}"),
                Err(e) => format!("err {e}"),
            }
        }
        "connect" => {
            let options = NativeSqliteOptions {
                busy_timeout: Duration::from_millis(100),
                ..base
            };
            match NativeSqliteBackend::connect_with(&url, options).await {
                Ok(db) => format!("ok {:?}", count(&db, "items").await?),
                Err(BackendError::Connection(e)) => format!("err {e}"),
                Err(e) => format!("err {e}"),
            }
        }
        "hold" => {
            let conn = rusqlite::Connection::open(&path).expect("open");
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
                .expect("read");
            println!("PROBE ready");
            let mut rest = Vec::new();
            drop(std::io::Read::read_to_end(&mut std::io::stdin(), &mut rest));
            format!("ok {n}")
        }
        "crash" => {
            let db = NativeSqliteBackend::connect_with(&url, base).await?;
            db.execute_script("CREATE TABLE items (n INTEGER)").await?;
            for n in 0..50 {
                db.execute_raw("INSERT INTO items VALUES (?)", vec![Value::Int(n)])
                    .await?;
            }
            let tx = db.begin(None).await?;
            for n in 50..100 {
                tx.execute_raw("INSERT INTO items VALUES (?)", vec![Value::Int(n)])
                    .await?;
            }
            tx.commit().await?;
            println!("PROBE committed");
            std::process::abort();
        }
        other => format!("unknown action {other}"),
    };
    println!("PROBE {report}");
    Ok(())
}
