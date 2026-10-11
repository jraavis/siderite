//! Field validators and model hooks over HTTP.

use crate::fixtures::*;
use serde_json::{Value, json};

// ---------------------------------------------------------------------
// HTTP: hooks
// ---------------------------------------------------------------------

#[tokio::test]
async fn reusable_field_validators_run_after_deserialization() {
    let c = client();
    let res = c
        .post_json("/reusable", &json!({"a": 3, "b": -2}))
        .await
        .unwrap();
    assert_eq!(
        errors(&res),
        vec![
            pair("body.a", "not_positive_even"),
            pair("body.b", "not_positive_even"),
        ]
    );
    let ok = c
        .post_json("/reusable", &json!({"a": 2, "b": 4}))
        .await
        .unwrap();
    assert_eq!(ok.status, 200);
}

#[tokio::test]
async fn hooks_run_in_declaration_order() {
    let c = client();
    // Field validators (declaration order), then model validators.
    let res = c
        .post_json("/ordered", &json!({"a": "x", "b": "x", "csv": "1,2,3"}))
        .await
        .unwrap();
    assert_eq!(
        errors(&res),
        vec![
            pair("body.a", "a_short"),
            pair("body.a", "is_x"),
            pair("body.b", "is_x"),
            pair("body", "same"),
            pair("body.csv", "long"),
        ]
    );
}

#[tokio::test]
async fn before_validators_see_raw_input() {
    let c = client();
    // The before-validator turns the string into a list before `Vec::prepare`.
    let ok = c
        .post_json("/ordered", &json!({"a": "abc", "b": "def", "csv": "1, 2"}))
        .await
        .unwrap();
    assert_eq!(ok.status, 200, "{}", ok.text());
    assert_eq!(ok.json::<Value>().unwrap()["csv"], json!(["1", "2"]));

    // The model before-validator runs first and, like every prepare-phase
    // error, prevents the after phase.
    let poisoned = c
        .post_json("/ordered", &json!({"a": "x", "b": "x", "poison": 1}))
        .await
        .unwrap();
    assert_eq!(errors(&poisoned), vec![pair("body", "poisoned")]);
}

#[tokio::test]
async fn generic_models_validate_nested_items_and_run_hooks() {
    let c = client();
    let bad = c
        .post_json(
            "/pages",
            &json!({"items": [{"qty": 1}, {"qty": 0}], "total": 2}),
        )
        .await
        .unwrap();
    assert_eq!(
        errors(&bad),
        vec![pair("body.items.1.qty", "greater_than_equal")]
    );

    let mismatch = c
        .post_json("/pages", &json!({"items": [{"qty": 1}], "total": 5}))
        .await
        .unwrap();
    assert_eq!(errors(&mismatch), vec![pair("body", "total_mismatch")]);

    let ok = c
        .post_json("/pages", &json!({"items": [{"qty": 1}], "total": 1}))
        .await
        .unwrap();
    assert_eq!(ok.status, 200, "{}", ok.text());
    // Computed fields work on generic models that opt in to hooks.
    assert_eq!(ok.json::<Value>().unwrap()["count"], 1);
}
