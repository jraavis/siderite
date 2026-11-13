#![allow(clippy::unwrap_used)]
use super::progress::PROGRESS_TABLE;
use super::test_backend::TestSqlite;
use super::*;
use crate::state::{FieldState, ModelState, SqlType};

async fn memory_db() -> Db {
    Db::new(TestSqlite::connect("sqlite::memory:").await.unwrap())
}

fn run_sql_migration(id: &str, bodies: &[(&str, &str)]) -> Migration {
    run_sql_migration_with(id, bodies, true)
}

fn run_sql_migration_with(id: &str, bodies: &[(&str, &str)], atomic: bool) -> Migration {
    Migration::new(
        id,
        Vec::new(),
        bodies
            .iter()
            .map(|(sql, reverse)| Operation::RunSQL {
                sql: (*sql).to_owned(),
                reverse_sql: Some((*reverse).to_owned()),
            })
            .collect(),
        atomic,
        Vec::new(),
    )
    .unwrap()
}

/// Whether the progress table exists: a resumable run creates it on the
/// way in, so its absence proves no progress was ever written.
async fn has_progress_table(db: &Db) -> bool {
    !db.raw_sql(
        &format!(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = '{PROGRESS_TABLE}'"
        ),
        Vec::new(),
    )
    .await
    .unwrap()
    .rows
    .is_empty()
}

/// A failed MySQL migration resumes after its committed operations.
///
/// `kind` is MySQL to exercise the resume path, but the statements run
/// on SQLite: `RunSQL` passes through and the MySQL-dialect
/// progress/history SQL (backticks, `VARCHAR`) is valid SQLite too.
#[tokio::test]
async fn mysql_resume_skips_committed_operations() {
    let db = memory_db().await;
    // `migrate` creates this before applying; direct `apply_ops_in_order`
    // calls must set it up themselves.
    db.execute_script(&create_history_sql(BackendKind::MySql))
        .await
        .unwrap();
    let migration = run_sql_migration(
        "0001_t",
        &[
            ("CREATE TABLE t (n INTEGER)", "DROP TABLE t"),
            ("CREATE TABLE u (n INTEGER)", "DROP TABLE u"),
            ("CREATE TABLE u (n INTEGER)", "DROP TABLE u"),
        ],
    );
    let state = ProjectState::new();
    let registry = crate::registry::MigrationRegistry::new();
    let err = apply_ops_in_order(&db, BackendKind::MySql, &migration, &state, &registry)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            MigrationError::MysqlPartial {
                index: 3,
                total: 3,
                ..
            }
        ),
        "{err:?}"
    );
    // Hand-repair the half-applied tail, then re-run: ops 1-2 are
    // skipped (replaying them would fail), op 3 runs, history lands.
    db.execute_script("DROP TABLE u").await.unwrap();
    // The duplicate CREATE failed without effects; the fixture explicitly
    // reconciles and clears only that script's recorded uncertainty.
    db.raw_execute(
        "DELETE FROM siderite_migration_intents WHERE migration_id = ?",
        vec![Value::Text(migration.id.clone())],
    )
    .await
    .unwrap();
    apply_ops_in_order(&db, BackendKind::MySql, &migration, &state, &registry)
        .await
        .unwrap();
    db.raw_sql("SELECT n FROM t", Vec::new()).await.unwrap();
    db.raw_sql("SELECT n FROM u", Vec::new()).await.unwrap();
    let history = db
        .raw_sql("SELECT id FROM siderite_migrations", Vec::new())
        .await
        .unwrap();
    assert_eq!(history.rows.len(), 1);
    let progress = load_progress(
        &db,
        BackendKind::MySql,
        "0001_t",
        &migration.checksum,
        PROGRESS_APPLY,
    )
    .await
    .unwrap();
    assert_eq!(progress, Progress::default());
}

/// A progress row written by a different version of the migration must
/// not be resumed: its indices point at other operations, so skipping
/// them would silently leave statements unapplied.
#[tokio::test]
async fn mysql_resume_refuses_edited_migration() {
    let db = memory_db().await;
    db.execute_script(&create_history_sql(BackendKind::MySql))
        .await
        .unwrap();
    let original = run_sql_migration(
        "0005_edit",
        &[
            ("CREATE TABLE a (n INTEGER)", "DROP TABLE a"),
            ("CREATE TABLE a (n INTEGER)", "DROP TABLE a"),
        ],
    );
    let state = ProjectState::new();
    let registry = crate::registry::MigrationRegistry::new();
    apply_ops_in_order(&db, BackendKind::MySql, &original, &state, &registry)
        .await
        .unwrap_err();
    let edited = run_sql_migration(
        "0005_edit",
        &[
            ("CREATE TABLE b (n INTEGER)", "DROP TABLE b"),
            ("CREATE TABLE c (n INTEGER)", "DROP TABLE c"),
        ],
    );
    assert_ne!(original.checksum, edited.checksum);
    let err = apply_ops_in_order(&db, BackendKind::MySql, &edited, &state, &registry)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("changed since a partial"), "{err}");
    // Nothing from the edited file ran: `b` would exist otherwise.
    assert!(db.raw_sql("SELECT n FROM b", Vec::new()).await.is_err());
}

/// Progress is per *statement*, not per operation: one operation that
/// renders several statements must resume at the next statement, or the
/// re-run replays the ones MySQL already committed and fails with
/// "already exists".
#[tokio::test]
async fn mysql_resume_skips_committed_statements_of_one_operation() {
    let db = memory_db().await;
    db.execute_script(&create_history_sql(BackendKind::MySql))
        .await
        .unwrap();
    // One `CreateModel` operation rendering two statements: the table and
    // an auto index for `code`. `mm_code_idx` already exists on `clash`,
    // so statement 2 fails on a live server (index names are global).
    // MySQL-dialect SQL runs on SQLite here, so no `AUTO_INCREMENT`.
    let pk = || FieldState {
        primary_key: true,
        ..FieldState::new("id", "id", SqlType::BigInt)
    };
    let model = ModelState {
        name: "Multi".into(),
        table: "mm".into(),
        fields: vec![
            pk(),
            FieldState {
                index: true,
                ..FieldState::new("code", "code", SqlType::BigInt)
            },
        ],
        indexes: Vec::new(),
        constraints: Vec::new(),
    };
    let migration = Migration::new(
        "0004_multi",
        Vec::new(),
        vec![Operation::CreateModel { model }],
        true,
        Vec::new(),
    )
    .unwrap();
    db.execute_script(
        "CREATE TABLE clash (n INTEGER);\n\
         CREATE INDEX mm_code_idx ON clash (n)",
    )
    .await
    .unwrap();
    let state = ProjectState::new();
    let registry = crate::registry::MigrationRegistry::new();
    let err = apply_ops_in_order(&db, BackendKind::MySql, &migration, &state, &registry)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            MigrationError::MysqlPartial {
                index: 2,
                total: 2,
                ..
            }
        ),
        "{err:?}"
    );
    let progress = load_progress(
        &db,
        BackendKind::MySql,
        "0004_multi",
        &migration.checksum,
        PROGRESS_APPLY,
    )
    .await
    .unwrap();
    assert_eq!(progress.ops, 0, "the operation is not finished");
    assert_eq!(progress.stmts, 1, "its first statement is committed");

    // The re-run must not replay `CREATE TABLE mm`: hand-fix statement 2
    // only and leave the committed table in place.
    db.raw_sql("SELECT id FROM mm", Vec::new()).await.unwrap();
    db.execute_script("DROP INDEX mm_code_idx").await.unwrap();
    apply_ops_in_order(&db, BackendKind::MySql, &migration, &state, &registry)
        .await
        .unwrap();
    let index = db
        .raw_sql(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'mm_code_idx'",
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(index.rows.len(), 1, "{index:?}");
    let progress = load_progress(
        &db,
        BackendKind::MySql,
        "0004_multi",
        &migration.checksum,
        PROGRESS_APPLY,
    )
    .await
    .unwrap();
    assert_eq!(progress, Progress::default());
}

/// A `RunRust` failure after committed statements is a partial failure
/// that records progress; a `RunRust` failure with nothing committed
/// passes through untouched.
#[tokio::test]
async fn mysql_rust_failure_after_ddl_is_partial() {
    let db = memory_db().await;
    let migration = Migration::new(
        "0002_data",
        Vec::new(),
        vec![
            Operation::RunSQL {
                sql: "CREATE TABLE t (n INTEGER)".into(),
                reverse_sql: Some("DROP TABLE t".into()),
            },
            Operation::RunRust {
                name: "boom".into(),
                backwards: None,
            },
        ],
        true,
        Vec::new(),
    )
    .unwrap();
    let state = ProjectState::new();
    let mut registry = crate::registry::MigrationRegistry::new();
    registry.register("boom", |_db| {
        Box::pin(async move { Err::<(), MigrationError>(MigrationError::usage("seed exploded")) })
    });
    let err = apply_ops_in_order(&db, BackendKind::MySql, &migration, &state, &registry)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            MigrationError::MysqlOpPartial {
                index: 2,
                total: 2,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(err.to_string().contains("RunRust boom"), "{err:?}");
    let progress = load_progress(
        &db,
        BackendKind::MySql,
        "0002_data",
        &migration.checksum,
        PROGRESS_APPLY,
    )
    .await
    .unwrap();
    assert_eq!(progress.ops, 1);
    assert_eq!(progress.stmts, 0);

    // Nothing committed before a first-operation `RunRust`: plain error.
    let lonely = Migration::new(
        "0003_lonely",
        Vec::new(),
        vec![Operation::RunRust {
            name: "missing".into(),
            backwards: None,
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    let err = apply_ops_in_order(
        &db,
        BackendKind::MySql,
        &lonely,
        &state,
        &crate::registry::MigrationRegistry::new(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, MigrationError::UnregisteredRust(_)),
        "{err:?}"
    );
}

/// An earlier `RunRust` that committed data is partial state even when no
/// DDL ran: a later `RunRust` failure must say so, not return a plain
/// error that suggests re-running from scratch.
#[tokio::test]
async fn mysql_rust_failure_after_committed_rust_is_partial() {
    let db = memory_db().await;
    let migration = Migration::new(
        "0005_rust_then_boom",
        Vec::new(),
        vec![
            Operation::RunRust {
                name: "seed".into(),
                backwards: None,
            },
            Operation::RunRust {
                name: "boom".into(),
                backwards: None,
            },
        ],
        true,
        Vec::new(),
    )
    .unwrap();
    let state = ProjectState::new();
    let mut registry = crate::registry::MigrationRegistry::new();
    registry.register("seed", |db| {
        Box::pin(async move {
            db.execute_script("CREATE TABLE seeded (n INTEGER)").await?;
            Ok::<(), MigrationError>(())
        })
    });
    registry.register("boom", |_db| {
        Box::pin(async move { Err::<(), MigrationError>(MigrationError::usage("seed exploded")) })
    });
    let err = apply_ops_in_order(&db, BackendKind::MySql, &migration, &state, &registry)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            MigrationError::MysqlOpPartial {
                index: 2,
                total: 2,
                ..
            }
        ),
        "{err:?}"
    );
    db.raw_sql("SELECT n FROM seeded", Vec::new())
        .await
        .unwrap();
}

/// Only a run that leaves committed statements behind can be resumed:
/// MySQL always, PostgreSQL only without its transaction.
#[test]
fn on_failure_follows_backend_and_atomic() {
    assert_eq!(
        OnFailure::of(BackendKind::MySql, true),
        OnFailure::ImplicitCommit
    );
    assert_eq!(
        OnFailure::of(BackendKind::MySql, false),
        OnFailure::ImplicitCommit,
        "MySQL commits DDL implicitly whatever `atomic` says"
    );
    assert_eq!(
        OnFailure::of(BackendKind::Postgres, false),
        OnFailure::Autocommit
    );
    assert_eq!(
        OnFailure::of(BackendKind::Postgres, true),
        OnFailure::Rollback,
        "a transactional PostgreSQL migration rolls back completely"
    );
    for atomic in [true, false] {
        assert_eq!(
            OnFailure::of(BackendKind::Sqlite, atomic),
            OnFailure::Rollback,
            "the SQLite run is the lock's transaction, so `atomic` changes nothing"
        );
    }
}

/// What counts as "an earlier statement committed" differs per backend:
/// MySQL rolls DML back with the statement, a non-transactional
/// PostgreSQL run autocommits everything.
#[test]
fn prior_committed_depends_on_the_backend() {
    // Nothing ran before the failure.
    for failure in [
        OnFailure::Rollback,
        OnFailure::ImplicitCommit,
        OnFailure::Autocommit,
    ] {
        assert!(!failure.prior_committed(false, false), "{failure:?}");
    }
    // Only DML ran before it.
    assert!(!OnFailure::ImplicitCommit.prior_committed(true, false));
    assert!(OnFailure::Autocommit.prior_committed(true, false));
    // DDL ran before it.
    assert!(OnFailure::ImplicitCommit.prior_committed(true, true));
    assert!(OnFailure::Autocommit.prior_committed(true, true));
    assert!(!OnFailure::Rollback.prior_committed(true, true));
}

/// An atomic PostgreSQL migration runs in a transaction, so it records no
/// progress: a re-run starts from scratch and a failure stays plain.
///
/// `kind` is PostgreSQL to exercise the decision, but the `RunSQL`
/// statements run on SQLite. The second operation creates the first one's
/// table, so it fails exactly where a resumable run would write progress.
#[tokio::test]
async fn atomic_postgres_writes_no_progress_rows() {
    let db = memory_db().await;
    db.execute_script(&create_history_sql(BackendKind::Postgres))
        .await
        .unwrap();
    let migration = run_sql_migration_with(
        "0006_atomic_pg",
        &[
            ("CREATE TABLE pg_t (n INTEGER)", "DROP TABLE pg_t"),
            ("CREATE TABLE pg_t (n INTEGER)", "DROP TABLE pg_t"),
        ],
        true,
    );
    let state = ProjectState::new();
    let registry = crate::registry::MigrationRegistry::new();
    let err = apply_ops_in_order(&db, BackendKind::Postgres, &migration, &state, &registry)
        .await
        .unwrap_err();
    assert!(
        !matches!(
            err,
            MigrationError::MysqlPartial { .. } | MigrationError::PostgresPartial { .. }
        ),
        "a transactional migration leaves nothing behind: {err:?}"
    );
    assert!(
        !has_progress_table(&db).await,
        "an atomic PostgreSQL migration records no progress"
    );
}

/// A SQLite run is one transaction whatever the migration's `atomic` flag
/// says, so it records no progress either.
#[tokio::test]
async fn sqlite_writes_no_progress_rows() {
    let db = memory_db().await;
    db.execute_script(&create_history_sql(BackendKind::Sqlite))
        .await
        .unwrap();
    let state = ProjectState::new();
    let registry = crate::registry::MigrationRegistry::new();
    for (id, table, atomic) in [
        ("0007_sqlite_atomic", "sq_a", true),
        ("0008_sqlite_plain", "sq_b", false),
    ] {
        let migration = run_sql_migration_with(
            id,
            &[(&format!("CREATE TABLE {table} (n INTEGER)"), "SELECT 1")],
            atomic,
        );
        apply_ops_in_order(&db, BackendKind::Sqlite, &migration, &state, &registry)
            .await
            .unwrap();
        assert!(!has_progress_table(&db).await, "{id} (atomic={atomic})");
        db.raw_sql(&format!("SELECT n FROM {table}"), Vec::new())
            .await
            .unwrap();
    }
    let history = db
        .raw_sql("SELECT id FROM siderite_migrations", Vec::new())
        .await
        .unwrap();
    assert_eq!(history.rows.len(), 2, "both migrations applied");
}

#[test]
fn history_table_ddl_follows_the_dialect() {
    let pg = create_history_sql(BackendKind::Postgres);
    assert!(pg.contains("\"id\" TEXT PRIMARY KEY"), "{pg}");
    let mysql = create_history_sql(BackendKind::MySql);
    assert!(mysql.contains("`id` VARCHAR(255) PRIMARY KEY"), "{mysql}");
    assert!(mysql.contains("`siderite_migrations`"), "{mysql}");
    assert!(!mysql.contains('"'), "{mysql}");
}

#[test]
fn history_statements_quote_per_backend() {
    assert_eq!(
        quote_star(BackendKind::MySql, HISTORY_TABLE),
        "`siderite_migrations`"
    );
    assert_eq!(quote_star(BackendKind::Sqlite, "id"), "\"id\"");
}

#[test]
fn implicit_commit_matches_ddl_first_keyword() {
    for sql in [
        "CREATE TABLE t (n INTEGER)",
        "  alter table t add column m integer",
        "-- a comment\nDROP INDEX i",
        "/* wrapped */ TRUNCATE t",
        "rename table a to b",
    ] {
        assert!(is_implicit_commit(sql), "{sql}");
    }
    for sql in [
        "SELECT 1",
        "INSERT INTO t VALUES (1)",
        "UPDATE t SET n = 2",
        "DELETE FROM t",
        "-- only a comment",
        "",
    ] {
        assert!(!is_implicit_commit(sql), "{sql}");
    }
}

/// Without earlier DDL the failure is plain, even past statement 1: the
/// "earlier DDL committed" claim would be wrong.
#[tokio::test]
async fn mysql_plain_error_without_prior_ddl() {
    let db = memory_db().await;
    db.execute_script(&create_history_sql(BackendKind::MySql))
        .await
        .unwrap();
    let migration = run_sql_migration(
        "0004_noddl",
        &[
            ("SELECT 1", "SELECT 1"),
            (
                "CREATE TABLE q_that_fails (n INTEGER PRIMARY KEY, n INTEGER)",
                "SELECT 1",
            ),
        ],
    );
    let state = ProjectState::new();
    let registry = crate::registry::MigrationRegistry::new();
    let err = apply_ops_in_order(&db, BackendKind::MySql, &migration, &state, &registry)
        .await
        .unwrap_err();
    assert!(
        !matches!(err, MigrationError::MysqlPartial { .. }),
        "no DDL committed, so no partial: {err:?}"
    );
}

#[tokio::test]
async fn corrupt_resume_indices_cannot_skip_unexecuted_work() {
    let migration = run_sql_migration(
        "0001_bounds",
        &[(
            "CREATE TABLE untouched (id INTEGER)",
            "DROP TABLE untouched",
        )],
    );
    let kind = BackendKind::MySql;
    for progress in [
        Progress { ops: 2, stmts: 0 },
        Progress { ops: 1, stmts: 1 },
        Progress { ops: 0, stmts: 2 },
    ] {
        let db = memory_db().await;
        db.execute_script(&create_history_sql(kind)).await.unwrap();
        db.execute_script(&progress::progress_ddl(kind))
            .await
            .unwrap();
        save_progress(
            &db,
            kind,
            &migration.id,
            &migration.checksum,
            PROGRESS_APPLY,
            progress,
        )
        .await
        .unwrap();
        let result = apply_ops_in_order(
            &db,
            kind,
            &migration,
            &ProjectState::default(),
            &MigrationRegistry::new(),
        )
        .await;
        assert!(result.is_err(), "{progress:?}");
        assert!(db.raw_sql("SELECT * FROM untouched", vec![]).await.is_err());
        let history = db
            .raw_sql("SELECT * FROM siderite_migrations", vec![])
            .await
            .unwrap();
        assert!(history.rows.is_empty());
    }
}
