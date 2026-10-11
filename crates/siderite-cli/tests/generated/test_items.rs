//! `GET /items` through the in-process test client.

use siderite_testkit::TestClient;

fn client() -> TestClient {
    TestClient::new(crate::app())
}

#[tokio::test]
async fn items_answers_with_json() {
    let response = client().get("/items").await.unwrap();
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.content_type(), Some("application/json"));
    let body: serde_json::Value = response.json().unwrap();
    assert!(body["message"].is_string(), "{body}");
}

#[tokio::test]
async fn items_rejects_other_methods() {
    let response = client().delete("/items").await.unwrap();
    assert_eq!(response.status, 405);
}

#[tokio::test]
async fn items_is_documented() {
    let document = crate::app().openapi().unwrap();
    let document = serde_json::to_value(document).unwrap();
    let operation = &document["paths"]["/items"]["get"];
    assert_eq!(operation["operationId"], "items_index");
}
