//! `CursorParams` and `CursorPage` through the HTTP stack, over SQLite.
#![allow(clippy::unwrap_used)]

use serde_json::{Value, json};
use siderite::orm::{Databases, ModelOps};
use siderite::prelude::*;
use siderite::{CursorPage, CursorParams};
use siderite_testkit::{TestClient, TestDatabase};

#[derive(Debug, Clone, Model)]
#[model(table = "items", ordering = ["id"])]
struct Item {
    #[field(primary_key, auto)]
    id: i64,
    name: String,
    kind: String,
}

#[derive(Debug, Serialize, Schema)]
struct ItemOut {
    id: i64,
    name: String,
}

/// List items by cursor, two per page by default, at most five.
#[get("/items")]
async fn list_items(
    State(dbs): State<Databases>,
    Query(params): Query<CursorParams<5, 2>>,
) -> ApiResult<Json<CursorPage<ItemOut>>> {
    let rows = dbs.objects::<Item>()?.order_by([Item::id.asc()]);
    let page = params.fetch(rows).await?;
    Ok(Json(page.map(|i| ItemOut {
        id: i.id,
        name: i.name,
    })))
}

/// The same list for one kind: the cursor is bound to that filter.
#[get("/kinds/{kind}/items")]
async fn list_kind(
    State(dbs): State<Databases>,
    Path(kind): Path<String>,
    Query(params): Query<CursorParams<5, 2>>,
) -> ApiResult<Json<CursorPage<ItemOut>>> {
    let rows = dbs.objects::<Item>()?.filter(Item::kind.eq(kind));
    let page = params.fetch(rows.order_by([Item::id.asc()])).await?;
    Ok(Json(page.map(|i| ItemOut {
        id: i.id,
        name: i.name,
    })))
}

async fn app() -> App {
    let test = TestDatabase::sqlite_memory().await.unwrap();
    let test = test.with_models(&[Item::META]).await.unwrap();
    for n in 1..=7 {
        let name = format!("item {n}");
        let kind = if n % 2 == 0 { "even" } else { "odd" }.to_owned();
        Item { id: 0, name, kind }.save(test.db()).await.unwrap();
    }
    App::new()
        .database("default", test.into_db())
        .routes(routes![list_items, list_kind])
}

async fn get(client: &TestClient, path: &str) -> (u16, Value) {
    let response = client.get(path).await.unwrap();
    (response.status.as_u16(), response.json().unwrap())
}

fn names(page: &Value) -> Vec<&str> {
    let items = page["items"].as_array().unwrap();
    items.iter().map(|i| i["name"].as_str().unwrap()).collect()
}

#[tokio::test]
async fn pages_chain_forward_and_back_without_a_total() {
    let client = TestClient::new(app().await);
    let (status, first) = get(&client, "/items").await;
    assert_eq!(status, 200);
    assert_eq!(names(&first), ["item 1", "item 2"]);
    assert!(first.get("total").is_none());
    assert_eq!(first["previous"], Value::Null);
    let next = first["next"].as_str().unwrap();
    let path = format!("/items?after={next}&limit=3");
    let (_, second) = get(&client, &path).await;
    assert_eq!(names(&second), ["item 3", "item 4", "item 5"]);
    let previous = second["previous"].as_str().unwrap();
    let (_, back) = get(&client, &format!("/items?before={previous}")).await;
    assert_eq!(names(&back), names(&first));
    let next = second["next"].as_str().unwrap();
    let path = format!("/items?after={next}&limit=5");
    let (_, last) = get(&client, &path).await;
    assert_eq!(names(&last), ["item 6", "item 7"]);
    assert_eq!(last["next"], Value::Null);
}

#[tokio::test]
async fn a_cursor_belongs_to_the_filter_that_issued_it() {
    let client = TestClient::new(app().await);
    let (_, odd) = get(&client, "/kinds/odd/items").await;
    assert_eq!(names(&odd), ["item 1", "item 3"]);
    let next = odd["next"].as_str().unwrap();
    let path = format!("/kinds/odd/items?after={next}");
    let (status, rest) = get(&client, &path).await;
    assert_eq!((status, names(&rest)), (200, vec!["item 5", "item 7"]));
    let path = format!("/kinds/even/items?after={next}");
    let (status, problem) = get(&client, &path).await;
    assert_eq!(status, 400);
    assert_eq!(problem["code"], "cursor.E003");
    assert_eq!(problem["detail"], "The pagination cursor is not valid.");
}

#[tokio::test]
async fn bad_tokens_are_400_with_a_stable_code() {
    let client = TestClient::new(app().await);
    let (status, problem) = get(&client, "/items?after=@@@").await;
    assert_eq!((status, &problem["code"]), (400, &json!("cursor.E001")));
    let (_, first) = get(&client, "/items").await;
    let next = first["next"].as_str().unwrap();
    // A forward token read as a backward one.
    let path = format!("/items?before={next}");
    let (status, problem) = get(&client, &path).await;
    assert_eq!((status, &problem["code"]), (400, &json!("cursor.E001")));
    assert_eq!(problem["status"], 400);
}

#[tokio::test]
async fn parameter_mistakes_are_422() {
    let client = TestClient::new(app().await);
    let (_, first) = get(&client, "/items").await;
    let next = first["next"].as_str().unwrap();
    let both = format!("after={next}&before={next}");
    for bad in [&both[..], "limit=0", "limit=6", "limit=abc", "limit=-1"] {
        let (status, _) = get(&client, &format!("/items?{bad}")).await;
        assert_eq!(status, 422, "{bad}");
    }
    let (status, _) = get(&client, "/items?limit=5").await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn the_document_states_the_parameters_and_the_cursors() {
    let app = app().await;
    let document = serde_json::to_value(app.openapi().unwrap()).unwrap();
    let get = &document["paths"]["/items"]["get"];
    let params = get["parameters"].as_array().unwrap();
    let find = |name: &str| params.iter().find(|p| p["name"] == name).unwrap();
    assert_eq!(find("limit")["schema"]["maximum"], 5);
    assert_eq!(find("after")["in"], "query");
    assert_eq!(find("before")["schema"]["type"], "string");
    let ok = &get["responses"]["200"]["content"]["application/json"];
    let body = &ok["schema"]["properties"];
    assert!(body.get("next").is_some() && body.get("previous").is_some());
    assert!(body.get("total").is_none());
    assert_eq!(
        body["items"]["items"]["$ref"],
        "#/components/schemas/ItemOut"
    );
}
