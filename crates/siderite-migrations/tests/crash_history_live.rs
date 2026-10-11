//! Owned-process kills around the migration-history row write.
//!
//! A child process migrates (or rolls back) through a backend wrapper that
//! aborts immediately before or after the history row is inserted (apply) or
//! deleted (unapply). The parent then checks what `inspect_recovery`
//! reports, what a plain retry does, and that the documented operator
//! procedure completes the work.
//!
//! CI skips `history_recovery_child` by name; run these with
//! `--include-ignored --test-threads=1`.

mod common;
mod crash_support;

use common::scratch::ScratchDb;
use crash_support::{CrashBackend, Hook, run_child, table_exists};
use siderite_backends::{mysql::MySqlBackend, postgres::PgBackend};
use siderite_migrations::state::{FieldState, ModelState, SqlType};
use siderite_migrations::{Migration, MigrationError, MigrationGraph};
use siderite_migrations::{Migrator, Operation, RecoveryReport};
use siderite_orm::{BackendKind, Db};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

type Failure = Box<dyn std::error::Error + Send + Sync>;
type Result = std::result::Result<(), Failure>;
type Scratch = Arc<ScratchDb>;
type GraphResult = std::result::Result<MigrationGraph, MigrationError>;
type ReportResult = std::result::Result<RecoveryReport, Failure>;
type Outcome = std::result::Result<(), MigrationError>;

const ID: &str = "0001_history_boundary";
const TABLES: [&str; 2] = ["hist_a", "hist_b"];

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

fn graph(atomic: bool, changed: bool) -> GraphResult {
    let mut ops: Vec<_> = TABLES.iter().map(|t| model(t)).collect();
    if changed {
        ops.push(model("hist_extra"));
    }
    let migration = Migration::new(ID, vec![], ops, atomic, vec![])?;
    MigrationGraph::build(vec![migration])
}

#[tokio::test]
#[ignore = "child helper: invoked only by owned history crash tests"]
async fn history_recovery_child() -> Result {
    // A no-op unless a parent test configured this process as the victim.
    let Ok(url) = std::env::var("SIDERITE_HISTORY_URL") else {
        return Ok(());
    };
    let flag = std::env::var("SIDERITE_HISTORY_FLAG")?.into();
    let spec = std::env::var("SIDERITE_HISTORY_SPEC")?;
    let mode = std::env::var("SIDERITE_HISTORY_MODE")?;
    let atomic = std::env::var("SIDERITE_HISTORY_ATOMIC")? == "true";
    let hook = Hook::parse(&spec, flag).ok_or("invalid crash spec")?;
    let db = if url.starts_with("postgres") {
        let inner = PgBackend::connect(&url).await?;
        Db::new(CrashBackend::new(inner, hook))
    } else {
        let inner = MySqlBackend::connect(&url).await?;
        Db::new(CrashBackend::new(inner, hook))
    };
    let graph = graph(atomic, false)?;
    let migrator = Migrator::new(&db, &graph);
    if mode == "rollback" {
        migrator.rollback(None, Some(1), false).await?;
    } else {
        migrator.migrate(None, false).await?;
    }
    Err("child run completed without reaching its crash point".into())
}

/// Run the child until it aborts at `spec`, and prove the hook fired.
async fn kill(s: &ScratchDb, atomic: bool, spec: String, op: &str) -> Result {
    let envs = [
        ("SIDERITE_HISTORY_URL", s.connection_url().to_owned()),
        ("SIDERITE_HISTORY_SPEC", spec),
        ("SIDERITE_HISTORY_MODE", op.to_owned()),
        ("SIDERITE_HISTORY_ATOMIC", atomic.to_string()),
    ];
    run_child("history_recovery_child", "SIDERITE_HISTORY_FLAG", &envs).await
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

async fn assert_tables(db: &Db, want: [bool; 2]) -> Result {
    for (table, want) in TABLES.iter().zip(want) {
        if table_exists(db, table).await? != want {
            return Err(format!("{table}: expected exists={want}").into());
        }
    }
    Ok(())
}

async fn report(db: &Db, atomic: bool) -> ReportResult {
    let graph = graph(atomic, false)?;
    Ok(Migrator::new(db, &graph).inspect_recovery().await?)
}

async fn forward(db: &Db, atomic: bool, changed: bool) -> Outcome {
    let graph = graph(atomic, changed)?;
    Migrator::new(db, &graph)
        .with_lock_timeout(Duration::from_secs(10))?
        .migrate(None, false)
        .await
        .map(|_| ())
}

async fn backward(db: &Db, atomic: bool) -> Outcome {
    let graph = graph(atomic, false)?;
    Migrator::new(db, &graph)
        .with_lock_timeout(Duration::from_secs(10))?
        .rollback(None, Some(1), false)
        .await
        .map(|_| ())
}

fn atomic_pg(db: &Db, atomic: bool) -> bool {
    atomic && db.capabilities().kind == BackendKind::Postgres
}

fn stale_entry(
    report: &RecoveryReport,
    direction: &str,
    history_applied: bool,
) -> std::result::Result<(), Failure> {
    let [entry] = report.partial.as_slice() else {
        return Err(format!("expected one progress entry: {report:?}").into());
    };
    assert_eq!(entry.migration_id, ID);
    assert_eq!(entry.direction, direction);
    // Every operation finished; only the history write is in question.
    assert_eq!(
        (entry.completed_operations, entry.completed_statements),
        (TABLES.len(), 0)
    );
    assert_eq!(entry.total_operations, Some(TABLES.len()));
    assert!(entry.all_operations_recorded());
    assert_eq!(entry.file_matches, Some(true));
    assert_eq!(entry.history_applied, history_applied);
    assert!(report.uncertain_steps.is_empty());
    Ok(())
}

fn assert_applied(report: &RecoveryReport, applied: bool) {
    let want: Vec<String> = if applied { vec![ID.into()] } else { vec![] };
    assert_eq!(report.applied, want);
}

async fn assert_clean(db: &Db, atomic: bool, applied: bool) -> Result {
    let report = report(db, atomic).await?;
    assert_applied(&report, applied);
    assert!(report.partial.is_empty() && report.uncertain_steps.is_empty());
    assert_tables(db, [applied, applied]).await
}

/// Operator repair: delete the stale progress row for `direction`, after the
/// catalog and history have been verified.
async fn delete_progress(db: &Db, direction: &str) -> Result {
    let sql = format!(
        "DELETE FROM siderite_migration_progress \
         WHERE migration_id = '{ID}' AND direction = '{direction}'"
    );
    db.raw_execute(&sql, vec![]).await?;
    Ok(())
}

fn text(outcome: &Outcome) -> String {
    outcome
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_default()
}

/// Apply direction: kill immediately before or after the history insert.
async fn apply_case(scratch: Scratch, atomic: bool, after: bool) -> Result {
    let db = &scratch.db;
    let point = if after {
        "after-history-insert"
    } else {
        "before-history-insert"
    };
    kill(&scratch, atomic, Hook::spec(point, "-", 0), "migrate").await?;
    if atomic_pg(db, atomic) {
        // Schema and history share one transaction: both rolled back.
        assert_clean(db, atomic, false).await?;
        forward(db, atomic, false).await?;
        return assert_clean(db, atomic, true).await;
    }
    assert_tables(db, [true, true]).await?;
    let before = report(db, atomic).await?;
    assert_applied(&before, after);
    stale_entry(&before, "apply", after)?;
    // A changed file is refused while recorded progress or history exists.
    let refused = text(&forward(db, atomic, true).await);
    let want = if after {
        "checksum mismatch for applied migration"
    } else {
        "changed since a partial apply"
    };
    assert!(refused.contains(want), "unexpected: {refused}");
    assert_eq!(report(db, atomic).await?, before);
    // A plain retry never replays the finished DDL: all operations are
    // recorded, so only the history row (if missing) is written.
    forward(db, atomic, false).await?;
    if !after {
        return assert_clean(db, atomic, true).await;
    }
    // Recorded success, but the stale progress row survives a no-op retry.
    assert_eq!(report(db, atomic).await?, before);
    // Operator procedure: the history row proves success; drop the progress.
    delete_progress(db, "apply").await?;
    assert_clean(db, atomic, true).await?;
    // The next rollback/apply cycle must not replay or skip anything.
    backward(db, atomic).await?;
    assert_clean(db, atomic, false).await?;
    forward(db, atomic, false).await?;
    assert_clean(db, atomic, true).await
}

/// Same, but the stale row is left for the next migration cycle to meet.
async fn stale_apply_then_cycle(scratch: Scratch, atomic: bool) -> Result {
    let db = &scratch.db;
    let spec = Hook::spec("after-history-insert", "-", 0);
    kill(&scratch, atomic, spec, "migrate").await?;
    let stale = report(db, atomic).await?;
    assert_applied(&stale, true);
    stale_entry(&stale, "apply", true)?;
    backward(db, atomic).await?;
    assert_clean(db, atomic, false).await?;
    forward(db, atomic, false).await?;
    assert_clean(db, atomic, true).await
}

/// A stale unapply row left by a kill after the history delete is cleared
/// by the next forward run instead of being mistaken for pending work.
async fn stale_unapply_then_forward(scratch: Scratch, atomic: bool) -> Result {
    let db = &scratch.db;
    forward(db, atomic, false).await?;
    let spec = Hook::spec("after-history-delete", "-", 0);
    kill(&scratch, atomic, spec, "rollback").await?;
    stale_entry(&report(db, atomic).await?, "unapply", false)?;
    forward(db, atomic, false).await?;
    assert_clean(db, atomic, true).await
}

/// Unapply direction: kill immediately before or after the history delete.
async fn rollback_case(scratch: Scratch, atomic: bool, after: bool) -> Result {
    let db = &scratch.db;
    forward(db, atomic, false).await?;
    assert_clean(db, atomic, true).await?;
    let point = if after {
        "after-history-delete"
    } else {
        "before-history-delete"
    };
    kill(&scratch, atomic, Hook::spec(point, "-", 0), "rollback").await?;
    if atomic_pg(db, atomic) {
        // The drops and the history delete roll back together.
        assert_clean(db, atomic, true).await?;
        backward(db, atomic).await?;
        return assert_clean(db, atomic, false).await;
    }
    assert_tables(db, [false, false]).await?;
    let before = report(db, atomic).await?;
    assert_applied(&before, !after);
    stale_entry(&before, "unapply", !after)?;
    if !after {
        let refused = text(&forward(db, atomic, true).await);
        let want = "checksum mismatch for applied migration";
        assert!(refused.contains(want), "unexpected: {refused}");
        assert_eq!(report(db, atomic).await?, before);
        // Plain retry resumes: no DDL is replayed (the tables are gone and
        // would fail), only the history row is deleted.
        backward(db, atomic).await?;
        return assert_clean(db, atomic, false).await;
    }
    // History already deleted: rollback has nothing to do and leaves the
    // stale row; a forward run clears the opposite direction.
    backward(db, atomic).await?;
    assert_eq!(report(db, atomic).await?, before);
    delete_progress(db, "unapply").await?;
    assert_clean(db, atomic, false).await?;
    forward(db, atomic, false).await?;
    assert_clean(db, atomic, true).await
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
    pg pg_atomic_apply_before_insert => |s| apply_case(s, true, false);
    pg pg_atomic_apply_after_insert => |s| apply_case(s, true, true);
    pg pg_atomic_rollback_before_delete => |s| rollback_case(s, true, false);
    pg pg_atomic_rollback_after_delete => |s| rollback_case(s, true, true);
    pg pg_nonatomic_apply_before_insert => |s| apply_case(s, false, false);
    pg pg_nonatomic_apply_after_insert => |s| apply_case(s, false, true);
    pg pg_nonatomic_rollback_before_delete =>
        |s| rollback_case(s, false, false);
    pg pg_nonatomic_rollback_after_delete => |s| rollback_case(s, false, true);
    pg pg_nonatomic_stale_apply_cycle => |s| stale_apply_then_cycle(s, false);
    pg pg_nonatomic_stale_unapply_cycle =>
        |s| stale_unapply_then_forward(s, false);
    my mysql_stale_unapply_cycle => |s| stale_unapply_then_forward(s, true);
    my mysql_apply_before_insert => |s| apply_case(s, true, false);
    my mysql_apply_after_insert => |s| apply_case(s, true, true);
    my mysql_rollback_before_delete => |s| rollback_case(s, true, false);
    my mysql_rollback_after_delete => |s| rollback_case(s, true, true);
    my mysql_stale_apply_cycle => |s| stale_apply_then_cycle(s, true);
    // `atomic` cannot make MySQL DDL transactional; prove the flag
    // changes nothing at the history boundaries.
    my mysql_nonatomic_apply_before_insert =>
        |s| apply_case(s, false, false);
    my mysql_nonatomic_rollback_after_delete =>
        |s| rollback_case(s, false, true);
}
