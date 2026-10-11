//! Run-level atomicity, concurrency and RunRust ordering.

use crate::common::{author_meta, temp_dir};
use crate::fixtures::memory_db;
use siderite_backends::sqlite::SqliteBackend;
use siderite_migrations::executor::Migrator;
use siderite_migrations::loader::{self, MigrationGraph};
use siderite_migrations::make_migrations;
use siderite_migrations::migration::Migration;
use siderite_migrations::operation::Operation;
use siderite_migrations::registry::MigrationRegistry;
use siderite_orm::Db;

/// A failing migration rolls the whole SQLite run back.
///
/// `with_lock` holds a single `BEGIN IMMEDIATE` transaction for the entire
/// run (the lock), so a later failure undoes earlier migrations too — unlike
/// PostgreSQL, where each migration commits on its own.
#[tokio::test]
async fn sqlite_migrate_run_is_all_or_nothing() {
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_t",
        Vec::new(),
        vec![Operation::RunSQL {
            sql: "CREATE TABLE t (n INTEGER)".into(),
            reverse_sql: Some("DROP TABLE t".into()),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    let second = Migration::new(
        "0002_bad",
        vec![first.id.clone()],
        vec![
            Operation::RunSQL {
                sql: "CREATE TABLE u (n INTEGER)".into(),
                reverse_sql: Some("DROP TABLE u".into()),
            },
            Operation::RunSQL {
                sql: "CREATE TABLE u (n INTEGER)".into(),
                reverse_sql: Some("DROP TABLE u".into()),
            },
        ],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &first).unwrap();
    loader::write_migration(&dir, &second).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap_err();
    assert!(
        db.raw_sql("SELECT n FROM t", vec![]).await.is_err(),
        "the first migration must roll back with the failed run"
    );
    let shown = Migrator::new(&db, &graph).show().await.unwrap();
    assert!(shown.iter().all(|(_, applied)| !applied));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two concurrent migrators must apply each migration once.
#[tokio::test]
async fn concurrent_sqlite_migrate_applies_once() {
    let dir = temp_dir();
    let db_path = dir.join("app.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let db = Db::new(SqliteBackend::connect(&url).await.unwrap());
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta()];
    make_migrations(models, &dir, None, false).unwrap().unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();

    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let run = |db: Db, graph: MigrationGraph, barrier: std::sync::Arc<tokio::sync::Barrier>| async move {
        barrier.wait().await;
        Migrator::new(&db, &graph).migrate(None, false).await
    };
    let a = tokio::spawn(run(db.clone(), graph.clone(), barrier.clone()));
    let b = tokio::spawn(run(db.clone(), graph.clone(), barrier));
    let ra = a.await.unwrap().unwrap();
    let rb = b.await.unwrap().unwrap();
    assert_eq!(
        ra.applied.len() + rb.applied.len(),
        1,
        "one replica applies, the other sees history: {ra:?} {rb:?}"
    );
    for r in [&ra, &rb] {
        // `sql` must describe what was planned under the lock, not the
        // pre-lock preview: the loser applies nothing, so it must report
        // no SQL either.
        if r.applied.is_empty() {
            assert!(
                r.sql.is_empty(),
                "loser must not report stale pre-lock SQL: {r:?}"
            );
        } else {
            assert!(!r.sql.is_empty(), "winner must report its SQL: {r:?}");
        }
    }
    let shown = Migrator::new(&db, &graph).show().await.unwrap();
    assert_eq!(shown.len(), 1);
    assert!(shown[0].1);
    db.raw_sql("SELECT COUNT(*) AS n FROM authors", vec![])
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// RunRust must run between the SQL operations that surround it.
#[tokio::test]
async fn run_rust_interleaves_with_sql() {
    let dir = temp_dir();
    let db = memory_db().await;
    let create = Migration::new(
        "0001_t",
        Vec::new(),
        vec![
            Operation::RunSQL {
                sql: "CREATE TABLE t (n INTEGER)".into(),
                reverse_sql: Some("DROP TABLE t".into()),
            },
            Operation::RunRust {
                name: "seed".into(),
                backwards: Some("unseed".into()),
            },
            Operation::RunSQL {
                sql: "CREATE TABLE u AS SELECT * FROM t WHERE n = 1".into(),
                reverse_sql: Some("DROP TABLE u".into()),
            },
        ],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &create).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let mut registry = MigrationRegistry::new();
    registry.register("seed", |db| {
        Box::pin(async move {
            db.raw_execute("INSERT INTO t (n) VALUES (1)", vec![])
                .await?;
            Ok(())
        })
    });
    registry.register("unseed", |_db| Box::pin(async { Ok(()) }));
    Migrator::new(&db, &graph)
        .with_registry(registry)
        .migrate(None, false)
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT n FROM u", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1, "seed must run before the second SQL");
    let _ = std::fs::remove_dir_all(&dir);
}
