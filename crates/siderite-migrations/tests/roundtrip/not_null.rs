//! NOT NULL probes against populated tables.

use crate::common::temp_dir;
use crate::fixtures::memory_db;
use siderite_migrations::SqlType;
use siderite_migrations::executor::Migrator;
use siderite_migrations::loader::{self, MigrationGraph};
use siderite_migrations::migration::Migration;
use siderite_migrations::operation::Operation;
use siderite_migrations::state::FieldState;
use siderite_orm::Value;

fn not_null_item() -> siderite_migrations::state::ModelState {
    siderite_migrations::state::ModelState {
        name: "Item".into(),
        table: "items".into(),
        fields: vec![
            FieldState {
                nullable: false,
                primary_key: true,
                auto: true,
                ..FieldState::new("id", "id", SqlType::Integer)
            },
            FieldState {
                nullable: true,
                ..FieldState::new("val", "val", SqlType::Text)
            },
        ],
        indexes: Vec::new(),
        constraints: Vec::new(),
    }
}

/// Apply `ops` as a follow-up to `0001_item` (with one row holding `val`).
async fn not_null_followup(
    val: Option<&str>,
    ops: Vec<Operation>,
) -> (Result<(), String>, siderite_orm::Db, std::path::PathBuf) {
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_item",
        Vec::new(),
        vec![Operation::CreateModel {
            model: not_null_item(),
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
    let value = val.map_or(Value::Null, |v| Value::Text(v.into()));
    db.raw_execute("INSERT INTO items (val) VALUES (?)", vec![value])
        .await
        .unwrap();
    let next = Migration::new("0002_next", vec![first.id.clone()], ops, true, Vec::new()).unwrap();
    loader::write_migration(&dir, &next).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let res = Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string());
    (res, db, dir)
}

#[tokio::test]
async fn add_not_null_field_without_default_refuses_populated_table() {
    let field = FieldState {
        nullable: false,
        ..FieldState::new("qty", "qty", SqlType::Integer)
    };
    let (res, db, dir) = not_null_followup(
        Some("a"),
        vec![Operation::AddField {
            model: "Item".into(),
            field,
        }],
    )
    .await;
    let msg = res.unwrap_err();
    assert!(msg.contains("items.qty"), "{msg}");
    let cols = db
        .raw_sql("SELECT name FROM pragma_table_info('items')", vec![])
        .await
        .unwrap();
    assert_eq!(cols.rows.len(), 2, "table must be unchanged");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn alter_to_not_null_refuses_existing_nulls() {
    let field = FieldState {
        nullable: false,
        ..FieldState::new("val", "val", SqlType::Text)
    };
    let (res, db, dir) = not_null_followup(
        None,
        vec![Operation::AlterField {
            model: "Item".into(),
            name: "val".into(),
            field,
        }],
    )
    .await;
    let msg = res.unwrap_err();
    assert!(msg.contains("items.val"), "{msg}");
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1, "row must survive");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The probe reads the in-migration state: a NOT NULL field added right
/// after `CreateModel` in the same migration hits an empty table.
#[tokio::test]
async fn add_not_null_field_after_create_model_in_same_migration() {
    let dir = temp_dir();
    let db = memory_db().await;
    let mut other = not_null_item();
    other.name = "Other".into();
    other.table = "others".into();
    let m = Migration::new(
        "0001_other",
        Vec::new(),
        vec![
            Operation::CreateModel { model: other },
            Operation::AddField {
                model: "Other".into(),
                field: FieldState {
                    nullable: false,
                    ..FieldState::new("qty", "qty", SqlType::Integer)
                },
            },
        ],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &m).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
