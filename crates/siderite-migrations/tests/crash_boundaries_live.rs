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
use crash_support::{CrashBackend, Hook};
use siderite_migrations::state::{FieldState, ModelState, SqlType};
use siderite_migrations::{Migration, MigrationError, MigrationGraph};
use siderite_migrations::{Migrator, Operation};
use siderite_orm::{BackendKind, Db};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use uuid::Uuid;

type Result = std::result::Result<(), Box<dyn std::error::Error>>;
type GraphResult = std::result::Result<MigrationGraph, MigrationError>;

const ID: &str = "0001_crash_boundary";

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

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
    let url = std::env::var("SIDERITE_BOUNDARY_URL")?;
    let flag = std::env::var("SIDERITE_BOUNDARY_FLAG")?.into();
    let spec = std::env::var("SIDERITE_BOUNDARY_SPEC")?;
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
    Migrator::new(&db, &graph).migrate(None, false).await?;
    Err("child migration completed without reaching its crash point".into())
}

/// Run the child until it aborts at `spec`, and prove the hook fired.
async fn crash_at(scratch: &ScratchDb, shape: Shape, spec: String) -> Result {
    let flag = std::env::temp_dir().join(format!("siderite-boundary-{}", Uuid::new_v4().simple()));
    let mut child = OwnedChild(
        Command::new(std::env::current_exe()?)
            .args(["--ignored", "--exact", "boundary_recovery_child"])
            .env("SIDERITE_BOUNDARY_URL", scratch.connection_url())
            .env("SIDERITE_BOUNDARY_FLAG", &flag)
            .env("SIDERITE_BOUNDARY_SPEC", spec)
            .env("SIDERITE_BOUNDARY_SQL", shape.sql.to_string())
            .env("SIDERITE_BOUNDARY_ATOMIC", shape.atomic.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let status = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Some(status) = child.0.try_wait()? {
                return Ok::<_, std::io::Error>(status);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    let fired = flag.exists();
    let _ = std::fs::remove_file(&flag);
    if status.success() || !fired {
        return Err(format!("child did not die at its hook: {status}").into());
    }
    Ok(())
}

async fn exists(db: &Db, table: &str) -> bool {
    db.raw_sql(&format!("SELECT * FROM {table}"), vec![])
        .await
        .is_ok()
}

async fn assert_tables(db: &Db, shape: Shape, want: [bool; 2]) -> Result {
    for (table, want) in shape.tables().iter().zip(want) {
        if exists(db, table).await != want {
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
async fn retry(db: &Db, shape: Shape) -> std::result::Result<(), MigrationError> {
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

async fn finish(scratch: ScratchDb, result: Result) -> Result {
    let cleanup = scratch.cleanup().await;
    result?;
    cleanup?;
    Ok(())
}

/// Crash after the first DDL statement returned, before progress is written.
async fn after_first_ddl(scratch: ScratchDb, sql: bool, atomic: bool) -> Result {
    let shape = Shape {
        sql,
        atomic,
        changed: false,
    };
    let result = async {
        let db = &scratch.db;
        let spec = Hook::spec("after-ddl", shape.tables()[0], 0);
        crash_at(&scratch, shape, spec).await?;
        let graph = shape.graph()?;
        let report = Migrator::new(db, &graph).inspect_recovery().await?;
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
        let blocked = retry(db, shape).await;
        if sql {
            assert!(matches!(
                blocked,
                Err(MigrationError::UncertainSqlStep { .. })
            ));
        } else {
            // Loud "already exists"; the migration never runs past it.
            assert!(blocked.is_err());
        }
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
    .await;
    finish(scratch, result).await
}

/// Crash after the second DDL statement: the first is recorded, this is not.
async fn after_second_ddl(scratch: ScratchDb, sql: bool) -> Result {
    let shape = Shape {
        sql,
        atomic: false,
        changed: false,
    };
    let result = async {
        let db = &scratch.db;
        let spec = Hook::spec("after-ddl", shape.tables()[1], 0);
        crash_at(&scratch, shape, spec).await?;
        let graph = shape.graph()?;
        let report = Migrator::new(db, &graph).inspect_recovery().await?;
        assert!(report.applied.is_empty());
        assert_eq!(report.partial.len(), 1);
        let entry = &report.partial[0];
        assert_eq!(
            (entry.completed_operations, entry.completed_statements),
            (1, 0)
        );
        assert_eq!(entry.file_matches, Some(true));
        assert_eq!(report.uncertain_steps.len(), usize::from(sql));
        assert_tables(db, shape, [true, true]).await?;
        let blocked = retry(db, shape).await;
        if sql {
            assert!(matches!(
                blocked,
                Err(MigrationError::UncertainSqlStep { .. })
            ));
        } else {
            assert!(blocked.is_err());
        }
        // A changed file never resumes from stale bookkeeping.
        let changed = Shape {
            changed: true,
            ..shape
        };
        let refused = retry(db, changed).await.err().map(|e| e.to_string());
        assert!(refused.is_some_and(|m| m.contains("changed since")));
        let stale = Migrator::new(db, &changed.graph()?)
            .inspect_recovery()
            .await?;
        assert_eq!(stale.partial[0].file_matches, Some(false));
        // Operator procedure: remove only the unrecorded second table.
        drop_table(db, shape.tables()[1]).await?;
        if sql {
            clear_intent(db).await?;
        }
        retry(db, shape).await?;
        assert_complete(db, shape).await
    }
    .await;
    finish(scratch, result).await
}

/// Crash right after a progress write: a generated retry resumes safely.
async fn after_progress(scratch: ScratchDb, sql: bool) -> Result {
    let shape = Shape {
        sql,
        atomic: false,
        changed: false,
    };
    let result = async {
        let db = &scratch.db;
        crash_at(&scratch, shape, Hook::spec("after-progress", "-", 0)).await?;
        let graph = shape.graph()?;
        let report = Migrator::new(db, &graph).inspect_recovery().await?;
        let entry = &report.partial[0];
        assert_eq!(
            (entry.completed_operations, entry.completed_statements),
            (0, 1)
        );
        assert_tables(db, shape, [true, false]).await?;
        assert_eq!(report.uncertain_steps.len(), usize::from(sql));
        db.raw_execute(
            &format!("INSERT INTO {} VALUES (7)", shape.tables()[0]),
            vec![],
        )
        .await?;
        if sql {
            // The script intent still blocks automatic resume.
            let blocked = retry(db, shape).await;
            assert!(matches!(
                blocked,
                Err(MigrationError::UncertainSqlStep { .. })
            ));
            // Progress proves the statement finished; clear the intent.
            clear_intent(db).await?;
        }
        retry(db, shape).await?;
        assert_complete(db, shape).await?;
        // The recorded first statement was skipped, not replayed.
        let rows = db
            .raw_sql(&format!("SELECT id FROM {}", shape.tables()[0]), vec![])
            .await?;
        assert_eq!(rows.rows.len(), 1);
        Ok(())
    }
    .await;
    finish(scratch, result).await
}

/// Crash before a script runs: the intent exists but no effect does.
async fn before_script(scratch: ScratchDb) -> Result {
    let shape = Shape {
        sql: true,
        atomic: false,
        changed: false,
    };
    let result = async {
        let db = &scratch.db;
        let spec = Hook::spec("before-ddl", shape.tables()[0], 0);
        crash_at(&scratch, shape, spec).await?;
        let graph = shape.graph()?;
        let report = Migrator::new(db, &graph).inspect_recovery().await?;
        assert!(report.applied.is_empty() && report.partial.is_empty());
        assert_eq!(report.uncertain_steps.len(), 1);
        assert_eq!(report.uncertain_steps[0].callback, None);
        assert_tables(db, shape, [false, false]).await?;
        let blocked = retry(db, shape).await;
        assert!(matches!(
            blocked,
            Err(MigrationError::UncertainSqlStep { .. })
        ));
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
    .await;
    finish(scratch, result).await
}

/// Crash after the intent is cleared: nothing is uncertain, resume is safe.
async fn after_intent_finish(scratch: ScratchDb) -> Result {
    let shape = Shape {
        sql: true,
        atomic: false,
        changed: false,
    };
    let result = async {
        let db = &scratch.db;
        let spec = Hook::spec("after-intent-finish", "-", 0);
        crash_at(&scratch, shape, spec).await?;
        let graph = shape.graph()?;
        let report = Migrator::new(db, &graph).inspect_recovery().await?;
        assert!(report.uncertain_steps.is_empty());
        assert_eq!(report.partial[0].completed_statements, 1);
        assert_tables(db, shape, [true, false]).await?;
        retry(db, shape).await?;
        assert_complete(db, shape).await
    }
    .await;
    finish(scratch, result).await
}

macro_rules! live {
    (pg $name:ident, |$s:ident| $body:expr) => {
        #[tokio::test]
        #[ignore = "requires PostgreSQL with private database creation rights"]
        async fn $name() -> Result {
            let scratch = ScratchDb::postgres().await?.ok_or("no scratch")?;
            {
                let $s = scratch;
                $body.await
            }
        }
    };
    (my $name:ident, |$s:ident| $body:expr) => {
        #[tokio::test]
        #[ignore = "requires MySQL with private database creation rights"]
        async fn $name() -> Result {
            let scratch = ScratchDb::mysql().await?.ok_or("no scratch")?;
            {
                let $s = scratch;
                $body.await
            }
        }
    };
}

live!(pg pg_atomic_generated_after_ddl, |s| { after_first_ddl(s, false, true) });
live!(pg pg_atomic_script_after_ddl, |s| { after_first_ddl(s, true, true) });
live!(pg pg_nonatomic_generated_after_ddl, |s| { after_first_ddl(s, false, false) });
live!(pg pg_nonatomic_script_after_ddl, |s| { after_first_ddl(s, true, false) });
live!(my mysql_generated_after_ddl, |s| after_first_ddl( s, false, true ));
live!(my mysql_script_after_ddl, |s| after_first_ddl( s, true, true ));
live!(pg pg_nonatomic_generated_after_second_ddl, |s| { after_second_ddl(s, false) });
live!(pg pg_nonatomic_script_after_second_ddl, |s| { after_second_ddl(s, true) });
live!(my mysql_generated_after_second_ddl, |s| { after_second_ddl(s, false) });
live!(my mysql_script_after_second_ddl, |s| { after_second_ddl(s, true) });
live!(pg pg_nonatomic_generated_after_progress, |s| { after_progress(s, false) });
live!(pg pg_nonatomic_script_after_progress, |s| { after_progress(s, true) });
live!(my mysql_generated_after_progress, |s| { after_progress(s, false) });
live!(my mysql_script_after_progress, |s| after_progress( s, true ));
live!(pg pg_nonatomic_script_before_ddl, |s| before_script(s));
live!(my mysql_script_before_ddl, |s| before_script(s));
live!(pg pg_nonatomic_script_after_intent_finish, |s| after_intent_finish(s));
live!(my mysql_script_after_intent_finish, |s| after_intent_finish(s));
