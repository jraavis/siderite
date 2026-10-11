//! Code the generators write compiles against this checkout and works.
//! The files under `tests/generated/` are the generators' exact output
//! (checked by their unit tests).

#![allow(clippy::unwrap_used)]

#[path = "generated/route_items.rs"]
mod items;

#[path = "generated/model_book.rs"]
mod book;

use siderite::prelude::*;
use siderite_testkit::{TestClient, TestDatabase};

#[tokio::test]
async fn the_generated_route_serves_and_is_documented() {
    let app = App::new().routes(items::routes());
    let document = app.openapi().unwrap();
    let text = serde_json::to_string(&document).unwrap();
    assert!(text.contains("\"operationId\":\"items_index\""), "{text}");
    assert!(text.contains("ItemsInfo"), "{text}");
    let client = TestClient::new(App::new().routes(items::routes()));
    let response = client.get("/items").await.unwrap();
    assert_eq!(response.status, 200);
    let body: serde_json::Value = response.json().unwrap();
    assert_eq!(body["message"], "items");
}

#[tokio::test]
async fn the_generated_model_migrates_and_round_trips() {
    let meta = <book::Book as orm::Model>::META;
    assert_eq!(meta.table, "books");
    let columns: Vec<&str> = meta.fields.iter().map(|f| f.column).collect();
    let want = ["id", "title", "pages", "views", "price", "in_stock"];
    assert_eq!(columns, want);
    let id = std::process::id();
    let dir = std::env::temp_dir().join(format!("siderite-model-{id}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    use siderite_migrations::make_migrations;
    let made = make_migrations(&[meta], &dir, None, false);
    assert!(
        made.unwrap().is_some(),
        "makemigrations writes one migration"
    );
    let test = TestDatabase::sqlite_memory().await.unwrap();
    let test = test.with_migrations(&dir).await.unwrap();
    let db = test.db();
    let mut row = book::Book {
        id: 0,
        title: "Dune".to_owned(),
        pages: None,
        views: 3,
        price: Some(9.5),
        in_stock: true,
    };
    row.save(db).await.unwrap();
    let rows = book::Book::objects(db).all().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].title.as_str(), rows[0].pages), ("Dune", None));
    assert!(rows[0].id > 0);
    let _ = std::fs::remove_dir_all(&dir);
}
