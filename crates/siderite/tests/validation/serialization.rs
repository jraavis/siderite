//! Computed fields, serializers and dump options.

use crate::fixtures::*;
use serde_json::{Value, json};
use siderite::validation::{ValidationContext, parse_value};

// ---------------------------------------------------------------------
// HTTP: serialization
// ---------------------------------------------------------------------

fn expected_user() -> Value {
    json!({
        "firstName": "Ada",
        "lastName": "LOVELACE",
        "level": {"value": 1},
        "theme": "light",
        "bio": null,
        "fullName": "Ada Lovelace",
        "initials": "AL",
    })
}

#[tokio::test]
async fn responses_include_computed_fields_and_serializers() {
    let res = client().get("/user").await.unwrap();
    assert_eq!(res.json::<Value>().unwrap(), expected_user());
}

#[tokio::test]
async fn nested_models_keep_computed_fields() {
    let c = client();
    let list = c.get("/users").await.unwrap().json::<Value>().unwrap();
    assert_eq!(list, json!([expected_user(), expected_user()]));
    let team = c.get("/team").await.unwrap().json::<Value>().unwrap();
    assert_eq!(team["lead"], expected_user());
    assert_eq!(team["members"][0], expected_user());
}

#[tokio::test]
async fn json_dump_options_apply() {
    let c = client();
    let slim = c.get("/user/slim").await.unwrap().json::<Value>().unwrap();
    let mut want = expected_user();
    want.as_object_mut().unwrap().remove("initials");
    want.as_object_mut().unwrap().remove("bio");
    assert_eq!(slim, want);

    let only = c.get("/user/only").await.unwrap().json::<Value>().unwrap();
    assert_eq!(
        only,
        json!({"firstName": "Ada", "fullName": "Ada Lovelace"})
    );
}

#[tokio::test]
async fn exclude_defaults_drops_fields_equal_to_their_default() {
    let c = client();
    let user = c
        .get("/user/defaults")
        .await
        .unwrap()
        .json::<Value>()
        .unwrap();
    // `level` (1) and `theme` ("light") equal their defaults; `bio` has a
    // serde default (`None`) too.
    assert!(user.get("theme").is_none(), "{user}");
    assert!(user.get("level").is_none(), "{user}");
    assert!(user.get("bio").is_none(), "{user}");
    assert_eq!(user["firstName"], "Ada");

    let cfg = c.get("/config").await.unwrap().json::<Value>().unwrap();
    assert_eq!(cfg, json!({}));
}

#[tokio::test]
async fn field_exclude_hides_the_field_from_dumps_only() {
    let user = client()
        .get("/user")
        .await
        .unwrap()
        .json::<Value>()
        .unwrap();
    assert!(user.get("password").is_none());
    // Plain Serde still sees it; `exclude` is a Dump concept.
    assert_eq!(
        serde_json::to_value(sample_user()).unwrap()["password"],
        "hunter2"
    );
    // Input still accepts it.
    let parsed: User = parse_value(
        json!({"firstName": "A", "lastName": "B", "password": "p"}),
        ValidationContext::new(),
    )
    .unwrap();
    assert_eq!(parsed.password, "p");
}

#[tokio::test]
async fn model_serializer_wraps_the_output() {
    let res = client().get("/enveloped").await.unwrap();
    assert_eq!(
        res.json::<Value>().unwrap(),
        json!({"data": {"id": 4}, "version": 1})
    );
}
