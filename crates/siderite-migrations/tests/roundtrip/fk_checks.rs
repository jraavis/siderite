//! SQLite rebuild foreign-key behaviour.

use crate::common::{author_meta, book_meta, temp_dir};
use crate::fixtures::memory_db;
use siderite_backends::sqlite::SqliteBackend;
use siderite_migrations::cli;
use siderite_migrations::executor::Migrator;
use siderite_migrations::loader::{self, MigrationGraph};
use siderite_migrations::make_migrations;
use siderite_migrations::migration::Migration;
use siderite_migrations::operation::Operation;
use siderite_migrations::registry::MigrationRegistry;
use siderite_migrations::state::ProjectState;
use siderite_orm::{Db, Value};

/// Rebuild of a parent table must not CASCADE-delete child rows.
///
/// SQLite ignores `PRAGMA foreign_keys` inside a transaction, and the
/// migrator wraps atomic SQLite migrations in one. The rebuild has to turn
/// FKs off on the *same* connection *before* `BEGIN`.
#[tokio::test]
async fn sqlite_rebuild_of_parent_keeps_cascade_children() {
    let dir = temp_dir();
    let db_path = dir.join("app.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let db = Db::new(SqliteBackend::connect(&url).await.unwrap());
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta(), book_meta()];
    let first = make_migrations(models, &dir, None, false).unwrap().unwrap();
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();

    db.raw_execute(
        "INSERT INTO authors (name) VALUES (?)",
        vec![Value::Text("Ann".into())],
    )
    .await
    .unwrap();
    db.raw_execute(
        "INSERT INTO books (title, author_id) VALUES (?, ?)",
        vec![Value::Text("Pigs".into()), Value::Int(1)],
    )
    .await
    .unwrap();

    let mut name = ProjectState::from_metas(&[author_meta()])
        .model("Author")
        .unwrap()
        .field("name")
        .unwrap()
        .clone();
    name.max_length = Some(200);
    let alter = Migration::new(
        "0002_widen_author_name",
        vec![first.id],
        vec![Operation::AlterField {
            model: "Author".into(),
            name: "name".into(),
            field: name,
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &alter).unwrap();
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();

    let books = db.raw_sql("SELECT title FROM books", vec![]).await.unwrap();
    assert_eq!(
        books.rows.len(),
        1,
        "ON DELETE CASCADE child rows must survive AlterField on the parent"
    );
    assert_eq!(
        books.rows[0].get("title"),
        Some(&Value::Text("Pigs".into()))
    );
    let authors = db
        .raw_sql("SELECT name FROM authors", vec![])
        .await
        .unwrap();
    assert_eq!(
        authors.rows[0].get("name"),
        Some(&Value::Text("Ann".into()))
    );
    let orphan = db
        .raw_execute(
            "INSERT INTO books (title, author_id) VALUES (?, ?)",
            vec![Value::Text("Ghost".into()), Value::Int(999)],
        )
        .await;
    assert!(
        orphan.is_err(),
        "foreign keys must still be enforced after the rebuild"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A pre-existing FK violation in an unrelated row must not block a rebuild.
///
/// The pre-commit `foreign_key_check` is scoped to *new* violations: the
/// orphan below predates the migration, so widening `authors.name` must
/// succeed and leave the orphan untouched.
#[tokio::test]
async fn sqlite_rebuild_ignores_preexisting_violations() {
    let dir = temp_dir();
    let db_path = dir.join("app.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let db = Db::new(SqliteBackend::connect(&url).await.unwrap());
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta(), book_meta()];
    let first = make_migrations(models, &dir, None, false).unwrap().unwrap();
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();

    db.raw_execute(
        "INSERT INTO authors (name) VALUES (?)",
        vec![Value::Text("Ann".into())],
    )
    .await
    .unwrap();
    // `PRAGMA foreign_keys` is per-connection, so the OFF/ON pair must wrap
    // the orphan insert in one script on one pooled connection.
    db.execute_script(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO books (title, author_id) VALUES ('Ghost', 999);
         PRAGMA foreign_keys = ON;",
    )
    .await
    .unwrap();

    let mut name = ProjectState::from_metas(&[author_meta()])
        .model("Author")
        .unwrap()
        .field("name")
        .unwrap()
        .clone();
    name.max_length = Some(200);
    let alter = Migration::new(
        "0002_widen_author_name",
        vec![first.id],
        vec![Operation::AlterField {
            model: "Author".into(),
            name: "name".into(),
            field: name,
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &alter).unwrap();
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();

    let ghosts = db
        .raw_sql("SELECT title FROM books WHERE title = 'Ghost'", vec![])
        .await
        .unwrap();
    assert_eq!(ghosts.rows.len(), 1, "pre-existing orphan must survive");
    let _ = std::fs::remove_dir_all(&dir);
}

/// When foreign keys were already off, the pre-commit check is skipped.
#[tokio::test]
async fn sqlite_rebuild_skips_check_when_fks_were_off() {
    let db = memory_db().await;
    db.execute_script(
        "CREATE TABLE authors (id INTEGER PRIMARY KEY AUTOINCREMENT, name VARCHAR(100) NOT NULL);
         CREATE TABLE books (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, author_id BIGINT NOT NULL REFERENCES authors (id) ON DELETE CASCADE, pages INTEGER);",
    )
    .await
    .unwrap();
    // Single-connection `:memory:` pool: this OFF persists for the migration.
    db.execute_script(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO authors (name) VALUES ('Ann');
         INSERT INTO books (title, author_id) VALUES ('Ghost', 999);",
    )
    .await
    .unwrap();

    let dir = temp_dir();
    let alter = Migration::new(
        "0001_widen",
        Vec::new(),
        vec![Operation::RunSQL {
            sql: "CREATE TABLE t (n INTEGER)".into(),
            reverse_sql: Some("DROP TABLE t".into()),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &alter).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    let ghosts = db
        .raw_sql("SELECT title FROM books WHERE title = 'Ghost'", vec![])
        .await
        .unwrap();
    assert_eq!(ghosts.rows.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `RunRust` on SQLite runs with foreign keys off, and the pre-commit
/// check rejects orphans created mid-migration.
///
/// The data-migration closure gets the rebuild connection, where `PRAGMA
/// foreign_keys` is off, so the orphan insert below succeeds. Commit then
/// fails with the offending table in the message instead of silently
/// cementing the orphan.
#[tokio::test]
async fn run_rust_sees_no_fk_enforcement_on_sqlite() {
    let dir = temp_dir();
    let db = memory_db().await;
    let create = Migration::new(
        "0001_t",
        Vec::new(),
        vec![
            Operation::RunSQL {
                sql: "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT NOT NULL)"
                    .into(),
                reverse_sql: Some("DROP TABLE authors".into()),
            },
            Operation::RunSQL {
                sql: "CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT NOT NULL, author_id BIGINT NOT NULL REFERENCES authors (id) ON DELETE CASCADE)"
                    .into(),
                reverse_sql: Some("DROP TABLE books".into()),
            },
            Operation::RunRust {
                name: "orphan".into(),
                backwards: None,
            },
        ],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &create).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let mut registry = MigrationRegistry::new();
    registry.register("orphan", |db| {
        Box::pin(async move {
            db.raw_execute(
                "INSERT INTO books (title, author_id) VALUES ('Ghost', 999)",
                vec![],
            )
            .await?;
            Ok(())
        })
    });
    let err = Migrator::new(&db, &graph)
        .with_registry(registry)
        .migrate(None, false)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("books"),
        "commit must name the offending table: {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The pre-commit check compares violating *rows*, not counts.
///
/// A migration that repairs one pre-existing orphan while adding another
/// keeps the violation count equal, so a count comparison would pass it. The
/// new orphan must still fail the commit.
#[tokio::test]
async fn sqlite_check_detects_a_new_violation_behind_a_fixed_one() {
    let dir = temp_dir();
    let db = memory_db().await;
    let create = Migration::new(
        "0001_t",
        Vec::new(),
        vec![
            Operation::RunSQL {
                sql: "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT NOT NULL)"
                    .into(),
                reverse_sql: Some("DROP TABLE authors".into()),
            },
            Operation::RunSQL {
                sql: "CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT NOT NULL, author_id BIGINT NOT NULL REFERENCES authors (id) ON DELETE CASCADE)"
                    .into(),
                reverse_sql: Some("DROP TABLE books".into()),
            },
        ],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &create).unwrap();
    Migrator::new(
        &db,
        &MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap(),
    )
    .migrate(None, false)
    .await
    .unwrap();
    // Baseline orphan, created with enforcement off on the single `:memory:`
    // connection and enforcement back on so the migration takes the check.
    // The row id is fixed: the replacement must differ from it, or it is the
    // same violation and correctly passes.
    db.execute_script(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO books (id, title, author_id) VALUES (1, 'Ghost', 999);
         PRAGMA foreign_keys = ON;",
    )
    .await
    .unwrap();

    let swap = Migration::new(
        "0002_swap_orphans",
        vec![create.id.clone()],
        vec![Operation::RunRust {
            name: "swap".into(),
            backwards: None,
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &swap).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let mut registry = MigrationRegistry::new();
    registry.register("swap", |db| {
        Box::pin(async move {
            db.raw_execute("DELETE FROM books WHERE id = 1", Vec::new())
                .await?;
            db.raw_execute(
                "INSERT INTO books (id, title, author_id) VALUES (2, 'Ghost', 999)",
                Vec::new(),
            )
            .await?;
            Ok(())
        })
    });
    let err = Migrator::new(&db, &graph)
        .with_registry(registry)
        .migrate(None, false)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("books") && msg.contains("new violation"),
        "an equal violation count must not pass: {err:?}"
    );
    // The rollback restores the baseline orphan (id 1), not the swap's (id 2).
    let orphans = db
        .raw_sql("SELECT id FROM books", Vec::new())
        .await
        .unwrap();
    assert_eq!(orphans.rows.len(), 1, "{orphans:?}");
    assert_eq!(orphans.rows[0].get("id"), Some(&Value::Int(1)));
    let _ = std::fs::remove_dir_all(&dir);
}
