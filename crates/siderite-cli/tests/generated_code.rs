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

/// The tests `generate test` writes, run against this file's `app()`.
#[path = "generated/test_items.rs"]
mod test_items;

#[path = "generated/test_book.rs"]
mod test_book;

/// The application the generated tests build: the generated route and
/// resource, registered as their instructions say.
fn app() -> App {
    App::new()
        .routes(items::routes())
        .routes(routes::book::routes())
}

use http::{Method, Request, header};
use models::book;

use siderite::Body;
use siderite::prelude::*;
use siderite_testkit::{TestClient, TestDatabase, TestResponse};
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

/// A JSON request with `method`, which the test client has no shortcut
/// for.
async fn send(
    client: &TestClient,
    method: Method,
    path: &str,
    body: &serde_json::Value,
) -> TestResponse {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    client.send(request).await.unwrap()
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
    let names = [
        "list_book",
        "create_book",
        "get_book",
        "replace_book",
        "update_book",
        "delete_book",
        "NewBook",
        "BookPatch",
        "BookOut",
    ];
    for id in names {
        assert!(text.contains(id), "{id}");
    }
    // Every PATCH field may be left out; the creation body needs some.
    let document = serde_json::to_value(app.openapi().unwrap()).unwrap();
    let schemas = &document["components"]["schemas"];
    let required = |name: &str| schemas[name]["required"].clone();
    assert_eq!(required("BookPatch"), serde_json::Value::Null);
    let new_book = required("NewBook");
    assert!(new_book.as_array().unwrap().contains(&"title".into()));
    let title = &schemas["BookPatch"]["properties"]["title"];
    assert_eq!(title["type"], "string", "{title}");
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

#[tokio::test]
async fn updates_keep_the_key_and_follow_absent_versus_null() {
    let test = migrated().await;
    let app = App::new()
        .database("default", test.db().clone())
        .routes(routes::book::routes());
    let client = TestClient::new(app);
    let body = serde_json::json!({
        "title": "Dune", "pages": 412, "views": 1, "price": 9.5,
        "in_stock": true,
    });
    let created = client.post_json("/books", &body).await.unwrap();
    let created: serde_json::Value = created.json().unwrap();
    let id = created["id"].as_i64().unwrap();
    let url = format!("/books/{id}");
    // PATCH: absent fields keep their value; `null` clears a nullable one.
    let patch = serde_json::json!({"views": 2, "pages": null});
    let patched = send(&client, Method::PATCH, &url, &patch).await;
    assert_eq!(patched.status, 200);
    let patched: serde_json::Value = patched.json().unwrap();
    assert_eq!(patched["views"], 2);
    assert_eq!(patched["pages"], serde_json::Value::Null);
    assert_eq!(patched["title"], "Dune");
    assert_eq!(patched["price"], 9.5);
    assert_eq!(patched["id"], id);
    // `null` for a required field, an empty title, the key or any other
    // unknown field is rejected and changes nothing.
    for bad in [
        serde_json::json!({"title": null}),
        serde_json::json!({"title": ""}),
        serde_json::json!({"id": id + 1}),
        serde_json::json!({"views": 3, "id": 7}),
        serde_json::json!({"__proto__": {"views": 9}}),
        serde_json::json!({"title; DROP TABLE books": "x"}),
    ] {
        let response = send(&client, Method::PATCH, &url, &bad).await;
        assert_eq!(response.status, 422, "{bad}");
    }
    let unchanged = client.get(&url).await.unwrap();
    let unchanged: serde_json::Value = unchanged.json().unwrap();
    assert_eq!(unchanged, patched);
    // PUT replaces every field; the creation rules apply.
    let replace = serde_json::json!({
        "title": "Emma", "views": 0, "in_stock": false,
    });
    let replaced = send(&client, Method::PUT, &url, &replace).await;
    assert_eq!(replaced.status, 200);
    let replaced: serde_json::Value = replaced.json().unwrap();
    assert_eq!(replaced["title"], "Emma");
    assert_eq!(replaced["price"], serde_json::Value::Null);
    assert_eq!(replaced["id"], id);
    let with_key = serde_json::json!({
        "id": 99, "title": "x", "views": 0, "in_stock": false,
    });
    let rejected = send(&client, Method::PUT, &url, &with_key).await;
    assert_eq!(rejected.status, 422);
    // Missing rows are 404 for every operation.
    let gone = "/books/999999";
    let patch = serde_json::json!({"views": 1});
    assert_eq!(send(&client, Method::PATCH, gone, &patch).await.status, 404);
    assert_eq!(send(&client, Method::PUT, gone, &replace).await.status, 404);
    assert_eq!(client.delete(gone).await.unwrap().status, 404);
    // DELETE answers 204, then the row is gone.
    assert_eq!(client.delete(&url).await.unwrap().status, 204);
    assert_eq!(client.get(&url).await.unwrap().status, 404);
    assert_eq!(client.delete(&url).await.unwrap().status, 404);
    let rows = book::Book::objects(test.db()).count().await.unwrap();
    assert_eq!(rows, 0);
}

thread_local! {
    /// `orm.query` spans opened on this thread.
    static QUERIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts `orm.query` spans (one per statement the ORM runs) per thread.
/// It is the process-wide subscriber, so callsite caching cannot hide
/// spans from it; a current-thread test sees only its own statements.
struct CountQueries;

impl tracing::Subscriber for CountQueries {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::Id {
        if span.metadata().name() == "orm.query" {
            QUERIES.with(|q| q.set(q.get() + 1));
        }
        tracing::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::Id, _: &tracing::Id) {}

    fn event(&self, _: &tracing::Event<'_>) {}

    fn enter(&self, _: &tracing::Id) {}

    fn exit(&self, _: &tracing::Id) {}
}

/// Statements run on this thread since the last call.
fn take_queries() -> usize {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let _ = tracing::subscriber::set_global_default(CountQueries);
    });
    QUERIES.with(|q| q.replace(0))
}

#[tokio::test(flavor = "current_thread")]
async fn lists_are_paged_ordered_and_take_two_queries() {
    take_queries();
    let test = migrated().await;
    for n in 0..23 {
        let mut row = book::Book {
            id: 0,
            title: format!("book {n}"),
            pages: None,
            views: n,
            price: None,
            in_stock: true,
        };
        row.save(test.db()).await.unwrap();
    }
    let app = App::new()
        .database("default", test.db().clone())
        .routes(routes::book::routes());
    let client = TestClient::new(app);
    for (url, len) in [
        ("/books", 20),
        ("/books?page=2&per_page=5", 5),
        ("/books?page=1&per_page=100", 23),
        ("/books?page=9", 0),
    ] {
        take_queries();
        let page = client.get(url).await.unwrap();
        assert_eq!(page.status, 200, "{url}");
        assert_eq!(take_queries(), 2, "{url}: a count and one fetch");
        let page: serde_json::Value = page.json().unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), len, "{url}");
        assert_eq!(page["total"], 23);
    }
    let second = client.get("/books?page=2&per_page=5").await.unwrap();
    let second: serde_json::Value = second.json().unwrap();
    let ids: Vec<i64> = second["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["id"].as_i64().unwrap())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "ordered by id");
    assert_eq!(second["items"][0]["title"], "book 5");
    assert_eq!(second["total_pages"], 5);
    for bad in ["per_page=101", "per_page=0", "page=0", "page=x"] {
        let response = client.get(&format!("/books?{bad}")).await.unwrap();
        assert_eq!(response.status, 422, "{bad}");
    }
}
