//! Code the generators write compiles against this checkout and works.
//! The files under `tests/generated/` are the generators' exact output
//! (checked by their unit tests).

#![allow(clippy::unwrap_used)]

#[path = "generated/route_items.rs"]
mod items;

#[path = "generated/model_book.rs"]
mod model_book;

#[path = "generated/resource_book.rs"]
mod resource_book;

/// The layout a generated project has: `crate::models::book`.
mod models {
    pub(crate) use super::model_book as book;
}

mod routes {
    pub(crate) use super::resource_book as book;
}

use models::book;

use siderite::prelude::*;
use siderite_testkit::{TestClient, TestDatabase};
use std::sync::atomic::AtomicU32;

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

/// A SQLite database with the generated model's migration applied.
async fn migrated() -> TestDatabase {
    use siderite_migrations::make_migrations;
    let meta = <book::Book as orm::Model>::META;
    let id = std::process::id();
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("siderite-model-{id}-{n}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let made = make_migrations(&[meta], &dir, None, false);
    assert!(
        made.unwrap().is_some(),
        "makemigrations writes one migration"
    );
    let test = TestDatabase::sqlite_memory().await.unwrap();
    let test = test.with_migrations(&dir).await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    test
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

#[tokio::test]
async fn the_generated_model_migrates_and_round_trips() {
    let meta = <book::Book as orm::Model>::META;
    assert_eq!(meta.table, "books");
    let columns: Vec<&str> = meta.fields.iter().map(|f| f.column).collect();
    let want = ["id", "title", "pages", "views", "price", "in_stock"];
    assert_eq!(columns, want);
    let test = migrated().await;
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
}

#[tokio::test]
async fn the_generated_resource_creates_fetches_and_validates() {
    let test = migrated().await;
    let app = App::new()
        .database("default", test.db().clone())
        .routes(routes::book::routes());
    let text = serde_json::to_string(&app.openapi().unwrap()).unwrap();
    for id in ["create_book", "get_book", "NewBook", "BookOut"] {
        assert!(text.contains(id), "{id}");
    }
    let client = TestClient::new(app);
    let body = serde_json::json!({
        "title": "Dune",
        "views": 0,
        "in_stock": true,
    });
    let created = client.post_json("/books", &body).await.unwrap();
    assert_eq!(created.status, 201);
    let created: serde_json::Value = created.json().unwrap();
    let id = created["id"].as_i64().unwrap();
    assert!(id > 0);
    assert_eq!(created["pages"], serde_json::Value::Null);
    let fetched = client.get(&format!("/books/{id}")).await.unwrap();
    assert_eq!(fetched.status, 200);
    let fetched: serde_json::Value = fetched.json().unwrap();
    assert_eq!(fetched, created);
    let missing = client.get("/books/999999").await.unwrap();
    assert_eq!(missing.status, 404);
    let not_a_number = client.get("/books/abc").await.unwrap();
    assert!(not_a_number.status.is_client_error());
    let invalid = [
        serde_json::json!({"title": "", "views": 0, "in_stock": true}),
        serde_json::json!({"views": 0, "in_stock": true}),
        serde_json::json!({"title": "x", "views": "many", "in_stock": true}),
        serde_json::json!({"title": "x", "views": 0, "in_stock": true,
                           "pages": 1.5}),
    ];
    for body in invalid {
        let response = client.post_json("/books", &body).await.unwrap();
        assert_eq!(response.status, 422, "{body}");
    }
    let rows = book::Book::objects(test.db()).count().await.unwrap();
    assert_eq!(rows, 1, "invalid requests stored nothing");
}
