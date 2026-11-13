//! Unconfirmed callbacks must never be replayed by ordinary registration.

use super::MigrationRegistry as Registry;
use super::test_backend::TestSqlite;
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn graph() -> std::result::Result<MigrationGraph, MigrationError> {
    let migration = Migration::new(
        "0001_intent",
        vec![],
        vec![
            Operation::RunSQL {
                sql: "CREATE TABLE effects (id INTEGER PRIMARY KEY)".into(),
                reverse_sql: Some("DROP TABLE effects".into()),
            },
            Operation::RunRust {
                name: "write".into(),
                backwards: Some("undo".into()),
            },
        ],
        true,
        vec![],
    )?;
    MigrationGraph::build(vec![migration])
}

#[tokio::test]
async fn callback_failure_blocks_replay_before_more_data() -> Result {
    let db = Db::new(TestSqlite::connect("sqlite::memory:").await?);
    let graph = graph()?;
    let migration = graph.get("0001_intent").ok_or("missing migration")?;
    db.execute_script(&create_history_sql(BackendKind::MySql))
        .await?;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let mut registry = Registry::new();
    registry.register("write", move |db| {
        let count = Arc::clone(&count);
        Box::pin(async move {
            count.fetch_add(1, Ordering::SeqCst);
            db.raw_execute("INSERT INTO effects DEFAULT VALUES", vec![])
                .await?;
            Err(MigrationError::state("injected after committed data"))
        })
    });
    assert!(apply(&db, migration, &registry).await.is_err());
    let report = Migrator::new(&db, &graph).inspect_recovery().await?;
    assert_eq!(report.uncertain_steps.len(), 1);
    assert_eq!(report.uncertain_steps[0].operation, 1);
    let retry = apply(&db, migration, &registry).await;
    assert!(matches!(
        retry,
        Err(MigrationError::UncertainRustStep { .. })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let effects = db.raw_sql("SELECT id FROM effects", vec![]).await?;
    assert_eq!(effects.rows.len(), 1);
    Ok(())
}

#[tokio::test]
async fn replay_safe_callback_finishes_without_duplicate_data() -> Result {
    let db = Db::new(TestSqlite::connect("sqlite::memory:").await?);
    let graph = graph()?;
    let migration = graph.get("0001_intent").ok_or("missing migration")?;
    db.execute_script(&create_history_sql(BackendKind::MySql))
        .await?;
    let attempts = Arc::new(AtomicUsize::new(0));
    let mut registry = Registry::new();
    registry.register_replay_safe("write", move |db| {
        let attempts = Arc::clone(&attempts);
        Box::pin(async move {
            db.raw_execute("INSERT OR IGNORE INTO effects VALUES (1)", vec![])
                .await?;
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(MigrationError::state("injected after data"));
            }
            Ok(())
        })
    });
    assert!(apply(&db, migration, &registry).await.is_err());
    apply(&db, migration, &registry).await?;
    let effects = db.raw_sql("SELECT id FROM effects", vec![]).await?;
    assert_eq!(effects.rows.len(), 1);
    let report = Migrator::new(&db, &graph).inspect_recovery().await?;
    assert!(report.partial.is_empty());
    assert!(report.uncertain_steps.is_empty());
    assert_eq!(report.applied, vec!["0001_intent"]);
    Ok(())
}

#[test]
fn ordinary_replacement_clears_replay_safety_declaration() {
    let mut registry = Registry::new();
    registry.register_replay_safe("write", |_| Box::pin(async { Ok(()) }));
    assert!(registry.is_replay_safe("write"));
    registry.register("write", |_| Box::pin(async { Ok(()) }));
    assert!(!registry.is_replay_safe("write"));
}

async fn apply(
    db: &Db,
    migration: &Migration,
    registry: &Registry,
) -> std::result::Result<(), MigrationError> {
    let state = ProjectState::default();
    let kind = BackendKind::MySql;
    apply_ops_in_order(db, kind, migration, &state, registry).await
}

#[tokio::test]
async fn partial_sql_script_blocks_duplicate_committed_data() -> Result {
    let db = Db::new(TestSqlite::connect("sqlite::memory:").await?);
    db.execute_script(&create_history_sql(BackendKind::MySql))
        .await?;
    db.execute_script("CREATE TABLE effects (id INTEGER)")
        .await?;
    let script = "INSERT INTO effects VALUES (1); SELECT * FROM missing";
    let migration = Migration::new(
        "0001_sql_intent",
        vec![],
        vec![Operation::RunSQL {
            sql: script.into(),
            reverse_sql: None,
        }],
        false,
        vec![],
    )?;
    let registry = Registry::new();
    assert!(apply(&db, &migration, &registry).await.is_err());
    assert!(matches!(
        apply(&db, &migration, &registry).await,
        Err(MigrationError::UncertainSqlStep { operation: 0, .. })
    ));
    let effects = db.raw_sql("SELECT id FROM effects", vec![]).await?;
    assert_eq!(effects.rows.len(), 1);
    let intents = intents::read(&db, Some(&migration.id)).await?;
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].callback, None);
    Ok(())
}

#[tokio::test]
async fn recorded_safe_callback_completion_clears_stale_intent() -> Result {
    let db = Db::new(TestSqlite::connect("sqlite::memory:").await?);
    let graph = graph()?;
    let migration = graph.get("0001_intent").ok_or("missing migration")?;
    let kind = BackendKind::MySql;
    db.execute_script(&create_history_sql(kind)).await?;
    db.execute_script("CREATE TABLE effects (id INTEGER PRIMARY KEY)")
        .await?;
    reset_progress_direction(&db, kind, &migration.id, "apply").await?;
    intents::begin(&db, migration, "apply", 1, Some("write")).await?;
    db.raw_execute("INSERT INTO effects VALUES (1)", vec![])
        .await?;
    save_progress(
        &db,
        kind,
        &migration.id,
        &migration.checksum,
        "apply",
        Progress { ops: 2, stmts: 0 },
    )
    .await?;
    let mut registry = Registry::new();
    registry.register_replay_safe("write", |_| {
        Box::pin(async {
            let error = MigrationError::state("completed callback must not run");
            Err(error)
        })
    });
    apply(&db, migration, &registry).await?;
    let report = Migrator::new(&db, &graph).inspect_recovery().await?;
    assert!(report.uncertain_steps.is_empty());
    assert_eq!(report.applied, vec!["0001_intent"]);
    let effects = db.raw_sql("SELECT id FROM effects", vec![]).await?;
    assert_eq!(effects.rows.len(), 1);
    Ok(())
}

#[tokio::test]
async fn recorded_safe_reverse_completion_does_not_rerun_callback() -> Result {
    let db = Db::new(TestSqlite::connect("sqlite::memory:").await?);
    let graph = graph()?;
    let migration = graph.get("0001_intent").ok_or("missing migration")?;
    let kind = BackendKind::MySql;
    db.execute_script(&create_history_sql(kind)).await?;
    let mut registry = Registry::new();
    registry.register("write", |_| Box::pin(async { Ok(()) }));
    apply(&db, migration, &registry).await?;
    reset_progress_direction(&db, kind, &migration.id, "unapply").await?;
    intents::begin(&db, migration, "unapply", 0, Some("undo")).await?;
    save_progress(
        &db,
        kind,
        &migration.id,
        &migration.checksum,
        "unapply",
        Progress { ops: 1, stmts: 0 },
    )
    .await?;
    registry.register_replay_safe("undo", |_| {
        Box::pin(async {
            let error = MigrationError::state("completed undo must not run");
            Err(error)
        })
    });
    let state = ProjectState::default();
    unapply_ops_in_order(&db, kind, migration, &state, &registry).await?;
    let report = Migrator::new(&db, &graph).inspect_recovery().await?;
    assert!(report.uncertain_steps.is_empty());
    assert!(report.applied.is_empty());
    assert!(db.raw_sql("SELECT id FROM effects", vec![]).await.is_err());
    Ok(())
}
