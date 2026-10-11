//! `/books` through the in-process test client, on a disposable
//! in-memory SQLite database created from the model.

use crate::models::book::Book;
use siderite::prelude::*;
use siderite_testkit::{TestClient, TestDatabase};

/// A client for the app with a fresh database as `default`; keep the
/// database alive as long as the client.
async fn client() -> (TestClient, TestDatabase) {
    let test = TestDatabase::sqlite_memory().await.unwrap();
    let test = test.with_models(&[Book::META]).await.unwrap();
    let client = TestClient::builder(crate::app())
        .with_database("default", test.db().clone())
        .build();
    (client, test)
}

#[tokio::test]
async fn book_round_trips() {
    let (client, _test) = client().await;
    let body = serde_json::json!({
        "title": "example",
        "pages": 7,
        "views": 7,
        "price": 1.5,
        "in_stock": true,
    });
    let created = client.post_json("/books", &body).await.unwrap();
    assert_eq!(created.status, 201, "{}", created.text());
    let created: serde_json::Value = created.json().unwrap();
    for (key, value) in body.as_object().unwrap() {
        assert_eq!(&created[key], value, "{key}");
    }
    let id = created["id"].as_i64().unwrap();
    let fetched = client.get(&format!("/books/{id}")).await.unwrap();
    assert_eq!(fetched.status, 200);
    assert_eq!(fetched.json::<serde_json::Value>().unwrap(), created);
    let page = client.get("/books").await.unwrap();
    let page: serde_json::Value = page.json().unwrap();
    assert_eq!(page["total"], 1);
}

#[tokio::test]
async fn book_rejects_invalid_input_and_unknown_ids() {
    let (client, test) = client().await;
    for body in [
        serde_json::json!({
            "id": 1,
            "title": "example",
            "pages": 7,
            "views": 7,
            "price": 1.5,
            "in_stock": true,
        }),
        serde_json::json!({
            "pages": 7,
            "views": 7,
            "price": 1.5,
            "in_stock": true,
        }),
    ] {
        let response = client.post_json("/books", &body).await.unwrap();
        assert_eq!(response.status, 422, "{body}");
    }
    let missing = client.get("/books/999999").await.unwrap();
    assert_eq!(missing.status, 404);
    let stored = Book::objects(test.db()).count().await.unwrap();
    assert_eq!(stored, 0, "rejected requests stored nothing");
}
