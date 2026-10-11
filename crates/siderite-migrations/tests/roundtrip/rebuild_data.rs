//! Data preservation across SQLite rebuilds and renames.

use crate::common::temp_dir;
use crate::fixtures::memory_db;
use siderite_migrations::SqlType;
use siderite_migrations::executor::Migrator;
use siderite_migrations::loader::{self, MigrationGraph};
use siderite_migrations::migration::Migration;
use siderite_migrations::operation::Operation;
use siderite_migrations::state::FieldState;
use siderite_orm::Value;

/// SQLite rebuild must fail closed on a lossy CAST instead of truncating.
///
/// `CAST('abc' AS INTEGER)` is `0`: copying it would destroy the value, so
/// the pre-copy probe aborts with the table and column named.
#[tokio::test]
async fn sqlite_lossy_cast_aborts_migration() {
    use siderite_migrations::state::ModelState;
    fn item(val_type: SqlType) -> ModelState {
        ModelState {
            name: "Item".into(),
            table: "items".into(),
            fields: vec![
                FieldState {
                    nullable: false,
                    primary_key: true,
                    auto: true,
                    ..FieldState::new("id", "id", SqlType::Integer)
                },
                FieldState::new("val", "val", val_type),
            ],
            indexes: Vec::new(),
            constraints: Vec::new(),
        }
    }
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_item",
        Vec::new(),
        vec![Operation::CreateModel {
            model: item(SqlType::Text),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &first).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    db.raw_execute("INSERT INTO items (val) VALUES ('abc')", vec![])
        .await
        .unwrap();

    let alter = Migration::new(
        "0002_val_to_int",
        vec![first.id.clone()],
        vec![Operation::AlterField {
            model: "Item".into(),
            name: "val".into(),
            field: FieldState::new("val", "val", SqlType::Integer),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &alter).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let err = Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("items") && msg.contains("val"),
        "error must name table/column: {err:?}"
    );
    // All-or-nothing: the row survives with its original value.
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows[0].get("val"), Some(&Value::Text("abc".into())));
    let shown = Migrator::new(&db, &graph).show().await.unwrap();
    assert!(
        shown
            .iter()
            .any(|(id, applied)| id == "0002_val_to_int" && !applied)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Lossless widenings still copy: `int -> REAL` and `anything -> TEXT`.
#[tokio::test]
async fn sqlite_lossless_casts_still_copy() {
    use siderite_migrations::state::ModelState;
    fn item(val_type: SqlType) -> ModelState {
        ModelState {
            name: "Item".into(),
            table: "items".into(),
            fields: vec![
                FieldState {
                    nullable: false,
                    primary_key: true,
                    auto: true,
                    ..FieldState::new("id", "id", SqlType::Integer)
                },
                FieldState::new("val", "val", val_type),
            ],
            indexes: Vec::new(),
            constraints: Vec::new(),
        }
    }
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_item",
        Vec::new(),
        vec![Operation::CreateModel {
            model: item(SqlType::Integer),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &first).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    db.raw_execute("INSERT INTO items (val) VALUES (5)", vec![])
        .await
        .unwrap();

    let to_real = Migration::new(
        "0002_val_to_real",
        vec![first.id.clone()],
        vec![Operation::AlterField {
            model: "Item".into(),
            name: "val".into(),
            field: FieldState::new("val", "val", SqlType::Real),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &to_real).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1);

    let to_text = Migration::new(
        "0003_val_to_text",
        vec![to_real.id.clone()],
        vec![Operation::AlterField {
            model: "Item".into(),
            name: "val".into(),
            field: FieldState::new("val", "val", SqlType::Text),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &to_text).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `RenameField` on SQLite must carry the column's rows into the new name.
///
/// Forward is a native `RENAME COLUMN`; rollback renames back. Either way
/// the stored value must survive instead of being dropped or nulled.
#[tokio::test]
async fn sqlite_rename_field_preserves_rows() {
    use siderite_migrations::state::ModelState;
    fn item(name: &str, column: &str) -> ModelState {
        ModelState {
            name: "Item".into(),
            table: "items".into(),
            fields: vec![
                FieldState {
                    nullable: false,
                    primary_key: true,
                    auto: true,
                    ..FieldState::new("id", "id", SqlType::Integer)
                },
                FieldState::new(name, column, SqlType::Text),
            ],
            indexes: Vec::new(),
            constraints: Vec::new(),
        }
    }
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_item",
        Vec::new(),
        vec![Operation::CreateModel {
            model: item("val", "val"),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &first).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    db.raw_execute("INSERT INTO items (val) VALUES ('hello')", vec![])
        .await
        .unwrap();

    let rename = Migration::new(
        "0002_rename_val",
        vec![first.id.clone()],
        vec![Operation::RenameField {
            model: "Item".into(),
            old_name: "val".into(),
            new_name: "label".into(),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &rename).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT label FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(
        rows.rows[0].get("label"),
        Some(&Value::Text("hello".into()))
    );

    Migrator::new(&db, &graph)
        .rollback(None, Some(1), false)
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].get("val"), Some(&Value::Text("hello".into())));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `AlterField` that renames the db column must copy old -> new on SQLite.
///
/// Same field name, different column: the rebuild `INSERT .. SELECT` has to
/// read the old column into the new one, and the rollback has to copy back.
#[tokio::test]
async fn sqlite_alter_field_column_rename_preserves_rows() {
    use siderite_migrations::state::ModelState;
    fn item(column: &str) -> ModelState {
        ModelState {
            name: "Item".into(),
            table: "items".into(),
            fields: vec![
                FieldState {
                    nullable: false,
                    primary_key: true,
                    auto: true,
                    ..FieldState::new("id", "id", SqlType::Integer)
                },
                FieldState::new("val", column, SqlType::Text),
            ],
            indexes: Vec::new(),
            constraints: Vec::new(),
        }
    }
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_item",
        Vec::new(),
        vec![Operation::CreateModel { model: item("val") }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &first).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    db.raw_execute("INSERT INTO items (val) VALUES ('hello')", vec![])
        .await
        .unwrap();

    let alter = Migration::new(
        "0002_rename_column",
        vec![first.id.clone()],
        vec![Operation::AlterField {
            model: "Item".into(),
            name: "val".into(),
            field: FieldState::new("val", "renamed", SqlType::Text),
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
    let rows = db
        .raw_sql("SELECT renamed FROM items", vec![])
        .await
        .unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(
        rows.rows[0].get("renamed"),
        Some(&Value::Text("hello".into()))
    );

    Migrator::new(&db, &graph)
        .rollback(None, Some(1), false)
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].get("val"), Some(&Value::Text("hello".into())));
    let _ = std::fs::remove_dir_all(&dir);
}
