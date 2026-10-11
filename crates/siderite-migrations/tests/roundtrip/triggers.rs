//! Trigger and hand-made index preservation.

use crate::common::temp_dir;
use crate::fixtures::memory_db;
use siderite_migrations::SqlType;
use siderite_migrations::executor::Migrator;
use siderite_migrations::loader::{self, MigrationGraph};
use siderite_migrations::migration::Migration;
use siderite_migrations::operation::Operation;
use siderite_migrations::state::FieldState;
use siderite_orm::Value;

/// SQLite rebuild must preserve triggers and hand-made indexes.
///
/// A trigger and an index created outside the migrator live in
/// `sqlite_master` but not in `ProjectState`; the rebuild drops them with
/// the old table unless the executor re-creates them afterwards. The model
/// index is recreated by the rebuild itself and must not be duplicated.
#[tokio::test]
async fn sqlite_rebuild_preserves_trigger_and_hand_index() {
    use siderite_migrations::state::{IndexState, ModelState};
    fn item() -> ModelState {
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
                FieldState::new("val", "val", SqlType::Text),
            ],
            indexes: vec![IndexState {
                name: "items_val_idx".into(),
                columns: vec!["val".into()],
                unique: false,
            }],
            constraints: Vec::new(),
        }
    }
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_item",
        Vec::new(),
        vec![Operation::CreateModel { model: item() }],
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
    db.execute_script(
        "CREATE TABLE items_audit (id INTEGER PRIMARY KEY, item_id INTEGER, val TEXT);
         CREATE TRIGGER items_val_trigger AFTER INSERT ON items BEGIN INSERT INTO items_audit (item_id, val) VALUES (NEW.id, NEW.val); END;
         CREATE INDEX items_val_hand_idx ON items (val);",
    )
    .await
    .unwrap();
    db.raw_execute("INSERT INTO items (val) VALUES ('hello')", vec![])
        .await
        .unwrap();

    let mut val = item().fields.into_iter().find(|f| f.name == "val").unwrap();
    val.max_length = Some(200);
    let alter = Migration::new(
        "0002_widen_val",
        vec![first.id.clone()],
        vec![Operation::AlterField {
            model: "Item".into(),
            name: "val".into(),
            field: val,
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

    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].get("val"), Some(&Value::Text("hello".into())));
    for name in ["items_val_trigger", "items_val_hand_idx", "items_val_idx"] {
        let found = db
            .raw_sql(
                "SELECT name FROM sqlite_master WHERE name = ?",
                vec![Value::Text(name.into())],
            )
            .await
            .unwrap();
        assert_eq!(found.rows.len(), 1, "`{name}` must survive the rebuild");
    }
    // The trigger still fires.
    db.raw_execute("INSERT INTO items (val) VALUES ('world')", vec![])
        .await
        .unwrap();
    let audit = db
        .raw_sql("SELECT val FROM items_audit ORDER BY id", vec![])
        .await
        .unwrap();
    assert_eq!(
        audit.rows.len(),
        2,
        "trigger must fire after the rebuild: {audit:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Removing a column a trigger reads must fail closed, not drop the trigger.
///
/// The rebuild would orphan `items_val_trigger` (`NEW.val` has no `val`
/// anymore), so the executor refuses before touching the table.
#[tokio::test]
async fn sqlite_rebuild_refuses_when_trigger_column_removed() {
    use siderite_migrations::state::ModelState;
    fn item() -> ModelState {
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
                FieldState::new("val", "val", SqlType::Text),
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
        vec![Operation::CreateModel { model: item() }],
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
    db.execute_script(
        "CREATE TABLE items_audit (id INTEGER PRIMARY KEY, item_id INTEGER, val TEXT);
         CREATE TRIGGER items_val_trigger AFTER INSERT ON items BEGIN INSERT INTO items_audit (item_id, val) VALUES (NEW.id, NEW.val); END;",
    )
    .await
    .unwrap();
    db.raw_execute("INSERT INTO items (val) VALUES ('hello')", vec![])
        .await
        .unwrap();

    let drop = Migration::new(
        "0002_drop_val",
        vec![first.id.clone()],
        vec![Operation::RemoveField {
            model: "Item".into(),
            name: "val".into(),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &drop).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let err = Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("items") && msg.contains("items_val_trigger"),
        "error must name table and trigger: {err:?}"
    );
    // All-or-nothing: the row and the trigger survive.
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows[0].get("val"), Some(&Value::Text("hello".into())));
    let found = db
        .raw_sql(
            "SELECT name FROM sqlite_master WHERE name = 'items_val_trigger'",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(found.rows.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
