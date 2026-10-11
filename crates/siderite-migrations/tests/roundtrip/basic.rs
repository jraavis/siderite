//! Round trip, dry-run, checksum and history basics.

use crate::common::{author_meta, book_meta, temp_dir};
use crate::fixtures::memory_db;
use siderite_migrations::cli::{self, ExitCode};
use siderite_migrations::executor::Migrator;
use siderite_migrations::loader::{self, MigrationGraph};
use siderite_migrations::migration::Migration;
use siderite_migrations::operation::Operation;
use siderite_migrations::state::FieldState;
use siderite_migrations::{MigrationError, SqlType, make_migrations};
use siderite_orm::Value;

#[tokio::test]
async fn sqlite_round_trip() {
    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta(), book_meta()];

    let first = make_migrations(models, &dir, None, false).unwrap().unwrap();
    assert!(first.id.starts_with("0001_"));
    assert!(dir.join(format!("{}.json", first.id)).exists());

    let db = memory_db().await;
    let code = cli::run(
        models,
        &db,
        &dir,
        ["migrate".to_owned(), "--dry-run".to_owned()],
    )
    .await
    .unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    // Dry-run must not create tables.
    let err = db.raw_sql("SELECT 1 FROM authors", vec![]).await;
    assert!(err.is_err());

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
        "INSERT INTO books (title, author_id, pages) VALUES (?, ?, ?)",
        vec![Value::Text("Pigs".into()), Value::Int(1), Value::Int(120)],
    )
    .await
    .unwrap();
    let rows = db.raw_sql("SELECT title FROM books", vec![]).await.unwrap();
    assert_eq!(rows.rows[0].get("title"), Some(&Value::Text("Pigs".into())));

    // Second migration: add isbn.
    let mut second = Migration::new(
        "0002_add_book_isbn",
        vec![first.id.clone()],
        vec![Operation::AddField {
            model: "Book".into(),
            field: FieldState {
                nullable: true,
                ..FieldState::new("isbn", "isbn", SqlType::Text)
            },
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    second.refresh_checksum().unwrap();
    loader::write_migration(&dir, &second).unwrap();

    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();
    db.raw_execute(
        "UPDATE books SET isbn = ? WHERE id = ?",
        vec![Value::Text("978-0".into()), Value::Int(1)],
    )
    .await
    .unwrap();
    let rows = db.raw_sql("SELECT isbn FROM books", vec![]).await.unwrap();
    assert_eq!(rows.rows[0].get("isbn"), Some(&Value::Text("978-0".into())));

    cli::run(
        models,
        &db,
        &dir,
        ["rollback".to_owned(), "--steps".to_owned(), "1".to_owned()],
    )
    .await
    .unwrap();
    let err = db.raw_sql("SELECT isbn FROM books", vec![]).await;
    assert!(err.is_err(), "isbn column should be gone after rollback");
    let rows = db.raw_sql("SELECT title FROM books", vec![]).await.unwrap();
    assert_eq!(rows.rows[0].get("title"), Some(&Value::Text("Pigs".into())));

    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let shown = Migrator::new(&db, &graph).show().await.unwrap();
    assert_eq!(shown[0].0, first.id);
    assert!(shown[0].1);
    assert_eq!(shown[1].0, second.id);
    assert!(!shown[1].1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn checksum_mismatch_of_applied_migration() {
    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta()];
    make_migrations(models, &dir, Some("initial"), false)
        .unwrap()
        .unwrap();
    let db = memory_db().await;
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();

    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    let path = &files[0];
    let mut migration: Migration =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    migration.operations.push(Operation::RunSQL {
        sql: "SELECT 1".into(),
        reverse_sql: None,
    });
    migration.refresh_checksum().unwrap();
    std::fs::write(path, serde_json::to_string_pretty(&migration).unwrap()).unwrap();

    let err = cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap_err();
    assert!(
        matches!(err, MigrationError::ChecksumMismatch { .. }),
        "{err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn irreversible_rollback_is_refused() {
    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta()];
    let first = make_migrations(models, &dir, Some("initial"), false)
        .unwrap()
        .unwrap();
    let irr = Migration::new(
        "0002_data",
        vec![first.id],
        vec![Operation::RunSQL {
            sql: "SELECT 1".into(),
            reverse_sql: None,
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &irr).unwrap();

    let db = memory_db().await;
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();
    let err = cli::run(
        models,
        &db,
        &dir,
        ["rollback".to_owned(), "--steps".to_owned(), "1".to_owned()],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, MigrationError::Irreversible { .. }),
        "{err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn dry_run_executes_nothing() {
    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta(), book_meta()];
    make_migrations(models, &dir, None, false).unwrap();
    let db = memory_db().await;
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let report = Migrator::new(&db, &graph)
        .migrate(None, true)
        .await
        .unwrap();
    assert!(report.dry_run);
    assert!(!report.planned.is_empty());
    assert!(report.applied.is_empty());
    assert!(db.raw_sql("SELECT 1 FROM authors", vec![]).await.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sqlite_rebuild_preserves_rows() {
    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta(), book_meta()];
    let first = make_migrations(models, &dir, None, false).unwrap().unwrap();
    let db = memory_db().await;
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();
    db.raw_execute(
        "INSERT INTO authors (name) VALUES (?)",
        vec![Value::Text("Bo".into())],
    )
    .await
    .unwrap();
    db.raw_execute(
        "INSERT INTO books (title, author_id) VALUES (?, ?)",
        vec![Value::Text("Go".into()), Value::Int(1)],
    )
    .await
    .unwrap();

    let drop_pages = Migration::new(
        "0002_drop_pages",
        vec![first.id],
        vec![Operation::RemoveField {
            model: "Book".into(),
            name: "pages".into(),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &drop_pages).unwrap();
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT title FROM books", vec![]).await.unwrap();
    assert_eq!(rows.rows[0].get("title"), Some(&Value::Text("Go".into())));
    assert!(db.raw_sql("SELECT pages FROM books", vec![]).await.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn unreadable_history_is_an_error_not_a_fresh_db() {
    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta(), book_meta()];
    make_migrations(models, &dir, None, false).unwrap().unwrap();
    let db = memory_db().await;
    // A history table that exists but cannot be read as history.
    db.raw_execute(
        "CREATE TABLE siderite_migrations (id INTEGER, checksum TEXT)",
        vec![],
    )
    .await
    .unwrap();
    db.raw_execute("INSERT INTO siderite_migrations VALUES (1, 'x')", vec![])
        .await
        .unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let err = Migrator::new(&db, &graph).show().await;
    assert!(err.is_err(), "expected an error, got {err:?}");
    // Nothing was replayed.
    assert!(db.raw_sql("SELECT 1 FROM authors", vec![]).await.is_err());
}
