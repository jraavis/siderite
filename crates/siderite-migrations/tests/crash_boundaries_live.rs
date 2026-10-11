//! Owned-process kills between executing DDL and recording its progress.
//!
//! A child process migrates through a backend wrapper that aborts at an exact
//! boundary (see `crash_support`). The parent then checks what inspection
//! reports, that a plain retry never silently replays unsafe work, and that
//! the documented operator procedure completes the migration.
//!
//! CI skips `boundary_recovery_child` by name; run these with
//! `--include-ignored --test-threads=1`.

mod common;
mod crash_support;

use common::scratch::ScratchDb;
use crash_support::{CrashBackend, Hook, run_child, table_exists};
use siderite_migrations::state::{FieldState, ModelState, SqlType};
use siderite_migrations::{Migration, MigrationError, MigrationGraph};
use siderite_migrations::{Migrator, Operation, RecoveryReport};
use siderite_orm::{BackendKind, Db};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

type Failure = Box<dyn std::error::Error + Send + Sync>;
type Result = std::result::Result<(), Failure>;
type Outcome = std::result::Result<(), MigrationError>;
type GraphResult = std::result::Result<MigrationGraph, MigrationError>;

const ID: &str = "0001_crash_boundary";

fn model(table: &str) -> Operation {
    let id = FieldState {
        primary_key: true,
        ..FieldState::new("id", "id", SqlType::BigInt)
    };
    Operation::CreateModel {
        model: ModelState {
            name: table.into(),
            table: table.into(),
            fields: vec![id],
            indexes: Vec::new(),
            constraints: Vec::new(),
        },
    }
}

fn script(table: &str) -> Operation {
    Operation::RunSQL {
        sql: format!("CREATE TABLE {table} (id INTEGER PRIMARY KEY)"),
        reverse_sql: Some(format!("DROP TABLE {table}")),
    }
}

/// How the fixture migration is built.
#[derive(Clone, Copy)]
struct Shape {
    /// Explicit `RunSQL` scripts instead of generated `CreateModel` DDL.
    sql: bool,
    /// Requested `atomic` flag (MySQL commits DDL implicitly regardless).
    atomic: bool,
    /// Adds a third operation, so the checksum differs from the original.
    changed: bool,
}

impl Shape {
    fn tables(self) -> [&'static str; 2] {
        match self.sql {
            true => ["crash_sql_a", "crash_sql_b"],
            false => ["crash_gen_a", "crash_gen_b"],
        }
    }

    fn graph(self) -> GraphResult {
        let make = |t: &str| if self.sql { script(t) } else { model(t) };
        let mut ops: Vec<_> = self.tables().iter().map(|t| make(t)).collect();
        if self.changed {
            ops.push(make("crash_extra"));
        }
        let migration = Migration::new(ID, vec![], ops, self.atomic, vec![])?;
        MigrationGraph::build(vec![migration])
    }
}

#[tokio::test]
#[ignore = "child helper: invoked only by owned boundary crash tests"]
async fn boundary_recovery_child() -> Result {
    // A no-op unless a parent test configured this process as the victim,
    // so `--include-ignored` without `--skip` stays green.
    let Ok(url) = std::env::var("SIDERITE_BOUNDARY_URL") else {
        return Ok(());
    };
    let flag = std::env::var("SIDERITE_BOUNDARY_FLAG")?.into();
    let spec = std::env::var("SIDERITE_BOUNDARY_SPEC")?;
    let mode = std::env::var("SIDERITE_BOUNDARY_MODE")?;
    let shape = Shape {
        sql: std::env::var("SIDERITE_BOUNDARY_SQL")? == "true",
        atomic: std::env::var("SIDERITE_BOUNDARY_ATOMIC")? == "true",
        changed: false,
    };
    let hook = Hook::parse(&spec, flag).ok_or("invalid crash spec")?;
    let db = if url.starts_with("postgres") {
        let inner = siderite_backends::postgres::PgBackend::connect(&url).await?;
        Db::new(CrashBackend::new(inner, hook))
    } else {
        let inner = siderite_backends::mysql::MySqlBackend::connect(&url).await?;
        Db::new(CrashBackend::new(inner, hook))
    };
    let graph = shape.graph()?;
    let migrator = Migrator::new(&db, &graph);
    if mode == "rollback" {
        migrator.rollback(None, Some(1), false).await?;
    } else {
        migrator.migrate(None, false).await?;
    }
    Err("child run completed without reaching its crash point".into())
}

/// Kill the child at `spec` while it migrates forward.
async fn crash_at(scratch: &ScratchDb, shape: Shape, spec: String) -> Result {
    crash_mode(scratch, shape, spec, "migrate").await
}

/// Run the child until it aborts at `spec`, and prove the hook fired.
async fn crash_mode(scratch: &ScratchDb, shape: Shape, spec: String, mode: &str) -> Result {
    let envs = [
        ("SIDERITE_BOUNDARY_URL", scratch.connection_url().to_owned()),
        ("SIDERITE_BOUNDARY_SPEC", spec),
        ("SIDERITE_BOUNDARY_MODE", mode.to_owned()),
        ("SIDERITE_BOUNDARY_SQL", shape.sql.to_string()),
        ("SIDERITE_BOUNDARY_ATOMIC", shape.atomic.to_string()),
    ];
    run_child("boundary_recovery_child", "SIDERITE_BOUNDARY_FLAG", &envs).await
}

async fn assert_tables(db: &Db, shape: Shape, want: [bool; 2]) -> Result {
    for (table, want) in shape.tables().iter().zip(want) {
        if table_exists(db, table).await? != want {
            return Err(format!("{table}: expected exists={want}").into());
        }
    }
    Ok(())
}

async fn drop_table(db: &Db, table: &str) -> Result {
    db.execute_script(&format!("DROP TABLE {table}")).await?;
    Ok(())
}

async fn clear_intent(db: &Db) -> Result {
    let sql = "DELETE FROM siderite_migration_intents \
               WHERE migration_id = '0001_crash_boundary'";
    db.raw_execute(sql, vec![]).await?;
    Ok(())
}

/// Retry through the normal API; returns the outcome without panicking.
async fn retry(db: &Db, shape: Shape) -> Outcome {
    let graph = shape.graph()?;
    Migrator::new(db, &graph)
        .with_lock_timeout(Duration::from_secs(10))?
        .migrate(None, false)
        .await
        .map(|_| ())
}

async fn assert_complete(db: &Db, shape: Shape) -> Result {
    assert_tables(db, shape, [true, true]).await?;
    let graph = shape.graph()?;
    let report = Migrator::new(db, &graph).inspect_recovery().await?;
    assert_eq!(report.applied, vec![ID]);
    assert!(report.partial.is_empty() && report.uncertain_steps.is_empty());
    Ok(())
}

fn atomic_pg(db: &Db, shape: Shape) -> bool {
    shape.atomic && db.capabilities().kind == BackendKind::Postgres
}

/// Run `case` on its own task so a failed assertion (a panic) still lets the
/// scratch database be dropped before the panic is re-raised.
async fn with_scratch<F, Fut>(scratch: ScratchDb, case: F) -> Result
where
    F: FnOnce(Arc<ScratchDb>) -> Fut,
    Fut: Future<Output = Result> + Send + 'static,
{
    let scratch = Arc::new(scratch);
    let joined = tokio::spawn(case(scratch.clone())).await;
    let scratch = Arc::into_inner(scratch).ok_or("scratch still shared")?;
    let cleanup = scratch.cleanup().await;
    match joined {
        Ok(result) => {
            result?;
            Ok(cleanup?)
        }
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => Err(e.into()),
    }
}

fn empty_shape(sql: bool, atomic: bool) -> Shape {
    Shape {
        sql,
        atomic,
        changed: false,
    }
}

async fn report(db: &Db, shape: Shape) -> std::result::Result<RecoveryReport, Failure> {
    let graph = shape.graph()?;
    Ok(Migrator::new(db, &graph).inspect_recovery().await?)
}

/// The retry failed with the database's own "already exists" error (PG
/// SQLSTATE 42P07, MySQL 1050), not a different failure.
fn assert_already_exists(outcome: &Outcome) {
    let text = outcome.as_ref().err().map(ToString::to_string);
    let text = text.unwrap_or_default().to_lowercase();
    assert!(
        text.contains("already exists"),
        "unexpected outcome: {text}"
    );
}

fn assert_uncertain(outcome: &Outcome) {
    let matched = matches!(outcome, Err(MigrationError::UncertainSqlStep { .. }));
    assert!(matched, "expected UncertainSqlStep, got {outcome:?}");
}

/// Retry that must be refused, leaving bookkeeping exactly as it was.
async fn assert_blocked(db: &Db, shape: Shape, uncertain: bool) -> Result {
    let before = report(db, shape).await?;
    let blocked = retry(db, shape).await;
    if uncertain {
        assert_uncertain(&blocked);
    } else {
        assert_already_exists(&blocked);
    }
    assert_eq!(
        report(db, shape).await?,
        before,
        "retry changed bookkeeping"
    );
    Ok(())
}

/// Crash after the first DDL statement returned, before progress is written.
async fn after_first_ddl(scratch: Arc<ScratchDb>, sql: bool, atomic: bool) -> Result {
    let shape = empty_shape(sql, atomic);
    let db = &scratch.db;
    let spec = Hook::spec("after-ddl", shape.tables()[0], 0);
    crash_at(&scratch, shape, spec).await?;
    let report = report(db, shape).await?;
    assert!(report.applied.is_empty() && report.partial.is_empty());
    if atomic_pg(db, shape) {
        // Transactional DDL: the kill rolled everything back.
        assert!(report.uncertain_steps.is_empty());
        assert_tables(db, shape, [false, false]).await?;
        retry(db, shape).await?;
        return assert_complete(db, shape).await;
    }
    // Implicit commit / autocommit: DDL applied, nothing recorded.
    assert_tables(db, shape, [true, false]).await?;
    assert_eq!(report.uncertain_steps.len(), usize::from(sql));
    // Generated DDL fails loudly on "already exists" and never runs past it.
    assert_blocked(db, shape, sql).await?;
    assert_tables(db, shape, [true, false]).await?;
    // Operator procedure: verify the catalog, undo the unrecorded DDL,
    // clear any intent, then retry.
    drop_table(db, shape.tables()[0]).await?;
    if sql {
        clear_intent(db).await?;
    }
    retry(db, shape).await?;
    assert_complete(db, shape).await
}

/// PostgreSQL `atomic: true` writes no intent or progress, so any kill point
/// leaves the database exactly as it was: nothing to reconcile, plain retry.
async fn atomic_pg_rollback(
    scratch: Arc<ScratchDb>,
    sql: bool,
    point: &str,
    table: usize,
) -> Result {
    let shape = empty_shape(sql, true);
    let db = &scratch.db;
    let spec = Hook::spec(point, shape.tables()[table], 0);
    crash_at(&scratch, shape, spec).await?;
    let report = report(db, shape).await?;
    assert!(report.applied.is_empty() && report.partial.is_empty());
    assert!(report.uncertain_steps.is_empty());
    assert_tables(db, shape, [false, false]).await?;
    retry(db, shape).await?;
    assert_complete(db, shape).await
}

/// Crash after the second DDL statement: the first is recorded, this is not.
async fn after_second_ddl(scratch: Arc<ScratchDb>, sql: bool) -> Result {
    let shape = empty_shape(sql, false);
    let db = &scratch.db;
    let spec = Hook::spec("after-ddl", shape.tables()[1], 0);
    crash_at(&scratch, shape, spec).await?;
    let report = report(db, shape).await?;
    assert!(report.applied.is_empty());
    assert_eq!(report.partial.len(), 1);
    let entry = &report.partial[0];
    let progress = (entry.completed_operations, entry.completed_statements);
    assert_eq!(progress, (1, 0));
    assert_eq!(entry.file_matches, Some(true));
    assert_eq!(report.uncertain_steps.len(), usize::from(sql));
    assert_tables(db, shape, [true, true]).await?;
    assert_blocked(db, shape, sql).await?;
    // A changed file never resumes from stale bookkeeping.
    let changed = Shape {
        changed: true,
        ..shape
    };
    let refused = retry(db, changed).await.err().map(|e| e.to_string());
    assert!(refused.is_some_and(|m| m.contains("changed since")));
    let stale = self::report(db, changed).await?;
    assert_eq!(stale.partial[0].file_matches, Some(false));
    // Operator procedure: remove only the unrecorded second table.
    drop_table(db, shape.tables()[1]).await?;
    if sql {
        clear_intent(db).await?;
    }
    retry(db, shape).await?;
    assert_complete(db, shape).await
}

/// Crash right after a progress write: a generated retry resumes safely.
async fn after_progress(scratch: Arc<ScratchDb>, sql: bool) -> Result {
    let shape = empty_shape(sql, false);
    let db = &scratch.db;
    crash_at(&scratch, shape, Hook::spec("after-progress", "-", 0)).await?;
    let report = report(db, shape).await?;
    let entry = &report.partial[0];
    let progress = (entry.completed_operations, entry.completed_statements);
    assert_eq!(progress, (0, 1));
    assert_tables(db, shape, [true, false]).await?;
    assert_eq!(report.uncertain_steps.len(), usize::from(sql));
    let insert = format!("INSERT INTO {} VALUES (7)", shape.tables()[0]);
    db.raw_execute(&insert, vec![]).await?;
    if sql {
        // The script intent still blocks automatic resume.
        assert_uncertain(&retry(db, shape).await);
        // Progress proves the statement finished; clear the intent.
        clear_intent(db).await?;
    }
    retry(db, shape).await?;
    assert_complete(db, shape).await?;
    // The recorded first statement was skipped, not replayed.
    let select = format!("SELECT id FROM {}", shape.tables()[0]);
    assert_eq!(db.raw_sql(&select, vec![]).await?.rows.len(), 1);
    Ok(())
}

/// Crash before a script runs: the intent exists but no effect does.
async fn before_script(scratch: Arc<ScratchDb>) -> Result {
    let shape = empty_shape(true, false);
    let db = &scratch.db;
    let spec = Hook::spec("before-ddl", shape.tables()[0], 0);
    crash_at(&scratch, shape, spec).await?;
    let report = report(db, shape).await?;
    assert!(report.applied.is_empty() && report.partial.is_empty());
    assert_eq!(report.uncertain_steps.len(), 1);
    assert_eq!(report.uncertain_steps[0].callback, None);
    assert_tables(db, shape, [false, false]).await?;
    assert_blocked(db, shape, true).await?;
    assert_tables(db, shape, [false, false]).await?;
    let changed = Shape {
        changed: true,
        ..shape
    };
    let refused = retry(db, changed).await.err().map(|e| e.to_string());
    assert!(refused.is_some_and(|m| m.contains("changed since")));
    // Operator verified nothing ran; clear the intent and retry.
    clear_intent(db).await?;
    retry(db, shape).await?;
    assert_complete(db, shape).await
}

/// Crash after the intent is cleared: nothing is uncertain, resume is safe.
async fn after_intent_finish(scratch: Arc<ScratchDb>) -> Result {
    let shape = empty_shape(true, false);
    let db = &scratch.db;
    let spec = Hook::spec("after-intent-finish", "-", 0);
    crash_at(&scratch, shape, spec).await?;
    let report = report(db, shape).await?;
    assert!(report.uncertain_steps.is_empty());
    assert_eq!(report.partial[0].completed_statements, 1);
    assert_tables(db, shape, [true, false]).await?;
    retry(db, shape).await?;
    assert_complete(db, shape).await
}

/// Rollback killed after its first DDL: the drop committed, the history row
/// and progress did not move, and a plain retry fails loudly.
async fn rollback_after_ddl(scratch: Arc<ScratchDb>) -> Result {
    let shape = empty_shape(false, false);
    let db = &scratch.db;
    retry(db, shape).await?;
    assert_complete(db, shape).await?;
    let spec = Hook::spec("after-ddl", shape.tables()[1], 0);
    crash_mode(&scratch, shape, spec, "rollback").await?;
    assert_tables(db, shape, [true, false]).await?;
    let before = report(db, shape).await?;
    assert_eq!(before.applied, vec![ID]);
    assert!(before.partial.is_empty() && before.uncertain_steps.is_empty());
    let graph = shape.graph()?;
    let again = Migrator::new(db, &graph)
        .rollback(None, Some(1), false)
        .await;
    assert!(again.is_err(), "retry must not silently succeed");
    assert_eq!(report(db, shape).await?, before);
    // Operator procedure: restore the dropped table, then roll back again.
    db.execute_script(&format!("CREATE TABLE {} (id INTEGER)", shape.tables()[1]))
        .await?;
    Migrator::new(db, &graph)
        .rollback(None, Some(1), false)
        .await?;
    assert_tables(db, shape, [false, false]).await?;
    assert!(report(db, shape).await?.applied.is_empty());
    Ok(())
}

macro_rules! live {
    ($($kind:ident $name:ident => $case:expr;)*) => {
        $( live!(@one $kind $name $case); )*
    };
    (@one pg $name:ident $case:expr) => {
        #[tokio::test]
        #[ignore = "requires PostgreSQL with private database creation rights"]
        async fn $name() -> Result {
            let scratch = ScratchDb::postgres().await?.ok_or("no scratch")?;
            with_scratch(scratch, $case).await
        }
    };
    (@one my $name:ident $case:expr) => {
        #[tokio::test]
        #[ignore = "requires MySQL with private database creation rights"]
        async fn $name() -> Result {
            let scratch = ScratchDb::mysql().await?.ok_or("no scratch")?;
            with_scratch(scratch, $case).await
        }
    };
}

live! {
    pg pg_atomic_generated_after_ddl => |s| after_first_ddl(s, false, true);
    pg pg_atomic_script_after_ddl => |s| after_first_ddl(s, true, true);
    pg pg_atomic_script_before_ddl =>
        |s| atomic_pg_rollback(s, true, "before-ddl", 0);
    pg pg_atomic_generated_after_second_ddl =>
        |s| atomic_pg_rollback(s, false, "after-ddl", 1);
    pg pg_atomic_script_after_second_ddl =>
        |s| atomic_pg_rollback(s, true, "after-ddl", 1);
    pg pg_nonatomic_generated_after_ddl => |s| after_first_ddl(s, false, false);
    pg pg_nonatomic_script_after_ddl => |s| after_first_ddl(s, true, false);
    my mysql_generated_after_ddl => |s| after_first_ddl(s, false, true);
    my mysql_script_after_ddl => |s| after_first_ddl(s, true, true);
    pg pg_nonatomic_generated_after_second_ddl => |s| after_second_ddl(s, false);
    pg pg_nonatomic_script_after_second_ddl => |s| after_second_ddl(s, true);
    my mysql_generated_after_second_ddl => |s| after_second_ddl(s, false);
    my mysql_script_after_second_ddl => |s| after_second_ddl(s, true);
    pg pg_nonatomic_generated_after_progress => |s| after_progress(s, false);
    pg pg_nonatomic_script_after_progress => |s| after_progress(s, true);
    my mysql_generated_after_progress => |s| after_progress(s, false);
    my mysql_script_after_progress => |s| after_progress(s, true);
    pg pg_nonatomic_script_before_ddl => before_script;
    my mysql_script_before_ddl => before_script;
    pg pg_nonatomic_script_after_intent_finish => after_intent_finish;
    my mysql_script_after_intent_finish => after_intent_finish;
    pg pg_nonatomic_generated_rollback_after_ddl => rollback_after_ddl;
    my mysql_generated_rollback_after_ddl => rollback_after_ddl;
}
