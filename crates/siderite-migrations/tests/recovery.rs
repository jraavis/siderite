//! Recovery inspection must not mutate fresh or damaged databases.

#[path = "common/backend.rs"]
mod backend;

use backend::TestSqlite;
use siderite_migrations::{MigrationGraph, Migrator};
use siderite_orm::{Db, Value};
use std::time::Duration;

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn fresh_inspection_creates_no_tables() -> Result {
    let db = Db::new(TestSqlite::connect("sqlite::memory:").await?);
    let graph = MigrationGraph::build(Vec::new())?;
    let migrator = Migrator::new(&db, &graph);
    let report = migrator.inspect_recovery().await?;
    assert!(report.applied.is_empty());
    assert!(report.partial.is_empty());
    let catalog = db.raw_sql("SELECT name FROM sqlite_master", vec![]).await?;
    assert!(catalog.rows.is_empty());
    let absent = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    let exit =
        siderite_migrations::cli::run(&[], &db, &absent, ["inspectmigrations".to_owned()]).await?;
    assert_eq!(exit, siderite_migrations::ExitCode::SUCCESS);
    assert!(!absent.exists());
    for value in ["0", "-1", "not-a-number"] {
        let args = ["migrate".to_owned(), format!("--lock-timeout-ms={value}")];
        assert!(
            siderite_migrations::cli::run(&[], &db, &absent, args)
                .await
                .is_err()
        );
    }
    let catalog = db.raw_sql("SELECT name FROM sqlite_master", vec![]).await?;
    assert!(catalog.rows.is_empty());
    assert!(
        Migrator::new(&db, &graph)
            .with_lock_timeout(Duration::ZERO)
            .is_err()
    );
    assert!(
        Migrator::new(&db, &graph)
            .with_lock_timeout(Duration::MAX)
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn damaged_progress_is_reported_without_repair() -> Result {
    let db = Db::new(TestSqlite::connect("sqlite::memory:").await?);
    let graph = MigrationGraph::build(Vec::new())?;
    db.execute_script(
        "CREATE TABLE siderite_migration_progress (
            migration_id TEXT, checksum TEXT, direction TEXT,
            op_index INTEGER, stmt_index INTEGER
        ); INSERT INTO siderite_migration_progress
            VALUES ('unknown', 'original', 'apply', 1, 2);",
    )
    .await?;
    let migrator = Migrator::new(&db, &graph);
    let report = migrator.inspect_recovery().await?;
    assert_eq!(report.partial.len(), 1);
    assert_eq!(report.partial[0].file_matches, None);
    assert_eq!(report.partial[0].completed_operations, 1);
    assert_eq!(report.partial[0].completed_statements, 2);
    for column in ["op_index", "stmt_index"] {
        db.raw_execute(
            &format!("UPDATE siderite_migration_progress SET {column} = -1"),
            vec![],
        )
        .await?;
        assert!(migrator.inspect_recovery().await.is_err());
        let stored = db
            .raw_sql(
                &format!("SELECT {column} FROM siderite_migration_progress"),
                vec![],
            )
            .await?;
        assert_eq!(stored.rows[0].get(column), Some(&Value::Int(-1)));
        db.raw_execute(
            &format!("UPDATE siderite_migration_progress SET {column} = 0"),
            vec![],
        )
        .await?;
    }
    db.raw_execute(
        "UPDATE siderite_migration_progress SET direction = 'invalid'",
        vec![],
    )
    .await?;
    assert!(migrator.inspect_recovery().await.is_err());
    Ok(())
}

#[tokio::test]
async fn scoped_execution_is_rejected_before_bookkeeping() -> Result {
    let db = Db::new(TestSqlite::connect("sqlite::memory:").await?);
    let graph = MigrationGraph::build(Vec::new())?;
    db.transaction::<_, _, (), siderite_migrations::MigrationError>(|tx| async move {
        assert!(tx.is_scoped());
        let migrator = Migrator::new(&tx, &graph);
        assert!(migrator.migrate(None, false).await.is_err());
        assert!(migrator.rollback(None, Some(1), false).await.is_err());
        let report = migrator.inspect_recovery().await?;
        assert!(report.applied.is_empty());
        let catalog = tx.raw_sql("SELECT name FROM sqlite_master", vec![]).await?;
        assert!(catalog.rows.is_empty());
        Ok(())
    })
    .await?;
    assert!(!db.is_scoped());
    Ok(())
}
