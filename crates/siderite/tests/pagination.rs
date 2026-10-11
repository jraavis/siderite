//! `PageParams` and `Page` through the HTTP stack, over SQLite.
#![allow(clippy::unwrap_used)]

use siderite::orm::{Databases, ModelOps};
use siderite::prelude::*;
use siderite::{Page, PageParams};
use siderite_testkit::{TestClient, TestDatabase};

#[derive(Debug, Clone, Model)]
#[model(table = "items", ordering = ["id"])]
struct Item {
    #[field(primary_key, auto)]
    id: i64,
    name: String,
}

#[derive(Debug, Serialize, Schema)]
struct ItemOut {
    id: i64,
    name: String,
}

#[derive(Debug, Serialize, Schema)]
struct Label {
    text: String,
}

/// List items, two per page by default, at most five.
#[get("/items")]
async fn list_items(
    State(dbs): State<Databases>,
    Query(params): Query<PageParams<5, 2>>,
) -> ApiResult<Json<Page<ItemOut>>> {
    let rows = dbs.objects::<Item>()?.order_by([Item::id.asc()]);
    let page = params.fetch(rows).await?;
    Ok(Json(page.map(|i| ItemOut {
        id: i.id,
        name: i.name,
    })))
}

/// A page of another item type in the same document.
#[get("/labels")]
async fn list_labels(Query(params): Query<PageParams>) -> Json<Page<Label>> {
    let label = Label {
        text: format!("page {}", params.page()),
    };
    Json(Page {
        items: vec![label],
        total: 1,
        page: params.page(),
        per_page: params.per_page(),
        total_pages: 1,
    })
}

async fn app() -> App {
    let test = TestDatabase::sqlite_memory().await.unwrap();
    let test = test.with_models(&[Item::META]).await.unwrap();
    for n in 1..=7 {
        let name = format!("item {n}");
        Item { id: 0, name }.save(test.db()).await.unwrap();
    }
    App::new()
        .database("default", test.into_db())
        .routes(routes![list_items, list_labels])
}

#[tokio::test]
async fn pages_are_bounded_counted_and_ordered() {
    let client = TestClient::new(app().await);
    let first = client.get("/items").await.unwrap();
    assert_eq!(first.status, 200);
    let first: serde_json::Value = first.json().unwrap();
    assert_eq!(first["per_page"], 2);
    assert_eq!(
        (first["total"].clone(), first["total_pages"].clone()),
        (7.into(), 4.into())
    );
    assert_eq!(first["items"][0]["name"], "item 1");
    let last = client.get("/items?page=4&per_page=2").await.unwrap();
    let last: serde_json::Value = last.json().unwrap();
    assert_eq!(last["items"].as_array().unwrap().len(), 1);
    assert_eq!(last["items"][0]["name"], "item 7");
    let past = client.get("/items?page=9").await.unwrap();
    let past: serde_json::Value = past.json().unwrap();
    assert_eq!(past["items"], serde_json::json!([]));
    assert_eq!(past["total"], 7);
    let max = client.get("/items?per_page=5").await.unwrap();
    assert_eq!(max.status, 200);
    for bad in ["page=0", "per_page=0", "per_page=6", "page=abc", "page=-2"] {
        let response = client.get(&format!("/items?{bad}")).await.unwrap();
        assert_eq!(response.status, 422, "{bad}");
    }
}

#[tokio::test]
async fn the_document_states_bounds_without_name_collisions() {
    let app = app().await;
    let document = serde_json::to_value(app.openapi().unwrap()).unwrap();
    let params = &document["paths"]["/items"]["get"]["parameters"];
    let per_page = params
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "per_page")
        .unwrap();
    assert_eq!(per_page["schema"]["maximum"], 5);
    assert_eq!(per_page["in"], "query");
    let schemas = document["components"]["schemas"].as_object().unwrap();
    assert!(schemas.contains_key("ItemOut") && schemas.contains_key("Label"));
    assert!(
        !schemas.keys().any(|k| k.starts_with("Page")),
        "{schemas:?}"
    );
    let response = &document["paths"]["/labels"]["get"]["responses"]["200"];
    let body = &response["content"]["application/json"]["schema"];
    let items = &body["properties"]["items"]["items"]["$ref"];
    assert_eq!(items, "#/components/schemas/Label");
}
