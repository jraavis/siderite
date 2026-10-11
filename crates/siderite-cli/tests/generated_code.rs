//! Code the generators write compiles against this checkout and works.
//! The files under `tests/generated/` are the generators' exact output
//! (checked by their unit tests).

#![allow(clippy::unwrap_used)]

#[path = "generated/route_items.rs"]
mod items;

use siderite::prelude::*;
use siderite_testkit::TestClient;

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
