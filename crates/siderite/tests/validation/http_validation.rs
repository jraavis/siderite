//! HTTP validation of request bodies and queries.

use crate::fixtures::*;
use serde_json::{Value, json};

// ---------------------------------------------------------------------
// HTTP: validation
// ---------------------------------------------------------------------

#[tokio::test]
async fn every_error_is_reported_with_body_locations() {
    let res = client()
        .post_json("/signup", &json!({"login": "b", "age": 10, "extra": 1}))
        .await
        .unwrap();
    assert_eq!(
        errors(&res),
        vec![
            pair("body.login", "too_short"),
            pair("body.age", "greater_than_equal"),
            pair("body.tags", "missing"),
            pair("body.address", "missing"),
            pair("body.extra", "extra_forbidden"),
        ]
    );
}

#[tokio::test]
async fn valid_signup_round_trips_and_aliases_apply() {
    let mut body = valid_signup();
    body["login"] = json!("  alice ");
    body.as_object_mut().unwrap().remove("userName");
    let res = client().post_json("/signup", &body).await.unwrap();
    assert_eq!(res.status, 200, "{}", res.text());
    // Stripped by `str_strip_whitespace`, serialized under the serde key.
    assert_eq!(res.json::<Value>().unwrap()["userName"], "alice");
}

#[tokio::test]
async fn populate_by_name_accepts_the_rust_field_name() {
    let mut body = valid_signup();
    body.as_object_mut().unwrap().remove("userName");
    body["user_name"] = json!("carol");
    let res = client().post_json("/signup", &body).await.unwrap();
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(res.json::<Value>().unwrap()["userName"], "carol");
}

#[tokio::test]
async fn missing_aliased_field_is_reported_under_the_alias() {
    let mut body = valid_signup();
    body.as_object_mut().unwrap().remove("userName");
    let res = client().post_json("/signup", &body).await.unwrap();
    assert_eq!(errors(&res), vec![pair("body.login", "missing")]);
}

#[tokio::test]
async fn nested_model_errors_carry_the_full_path() {
    let mut body = valid_signup();
    body["address"] = json!({"zip": "abc", "city": 5});
    let res = client().post_json("/signup", &body).await.unwrap();
    assert_eq!(
        errors(&res),
        vec![
            pair("body.address.zip", "pattern_mismatch"),
            pair("body.address.city", "string_type"),
        ]
    );
}

#[tokio::test]
async fn lists_of_models_use_index_locations() {
    let res = client()
        .post_json(
            "/basket",
            &json!({"items": [
                {"qty": 1, "code": "ok"},
                {"qty": 99, "code": ""},
                {"qty": "3", "code": "abcd"},
            ]}),
        )
        .await
        .unwrap();
    assert_eq!(
        errors(&res),
        vec![
            pair("body.items.1.qty", "less_than_equal"),
            pair("body.items.1.code", "too_short"),
            pair("body.items.2.code", "too_long"),
        ]
    );
}

#[tokio::test]
async fn strict_and_lax_models_differ_on_coercion() {
    let c = client();
    let lax = c.post_json("/lax", &json!({"n": "5"})).await.unwrap();
    assert_eq!(lax.json::<Value>().unwrap(), json!({"n": 5}));

    let strict = c.post_json("/strict", &json!({"n": "5"})).await.unwrap();
    assert_eq!(errors(&strict), vec![pair("body.n", "int_type")]);

    // A strict field inside a lax model: only that field is strict.
    let field = c
        .post_json("/field_strict", &json!({"n": "5", "m": "6"}))
        .await
        .unwrap();
    assert_eq!(errors(&field), vec![pair("body.n", "int_type")]);
}

#[tokio::test]
async fn string_transforms_apply_before_checks() {
    let c = client();
    let res = c
        .post_json("/lower", &json!({"name": "  HeLLo "}))
        .await
        .unwrap();
    assert_eq!(res.json::<Value>().unwrap(), json!({"name": "hello"}));
    let res = c.post_json("/upper", &json!({"name": "hi"})).await.unwrap();
    assert_eq!(res.json::<Value>().unwrap(), json!({"name": "HI"}));
}

#[tokio::test]
async fn query_strings_coerce_leniently_and_report_query_locations() {
    let c = client();
    let ok = c
        .get("/search?q=rust&limit=25&tags=a&tags=b&active=yes")
        .await
        .unwrap();
    assert_eq!(
        ok.json::<Value>().unwrap(),
        json!({"q": "rust", "limit": 25, "tags": ["a", "b"], "active": true})
    );
    let defaults = c.get("/search?q=x").await.unwrap();
    assert_eq!(defaults.json::<Value>().unwrap()["tags"], json!([]));
    let bad = c.get("/search?limit=1000&active=maybe").await.unwrap();
    assert_eq!(
        errors(&bad),
        vec![
            pair("query.q", "missing"),
            pair("query.limit", "less_than_equal"),
            pair("query.active", "bool_parsing"),
        ]
    );
}

#[tokio::test]
async fn unit_enums_are_checked_against_their_serde_names() {
    let c = client();
    let ok = c
        .post_json("/palette", &json!({"main": "red", "accent": "green"}))
        .await
        .unwrap();
    assert_eq!(ok.status, 200, "{}", ok.text());
    let bad = c
        .post_json("/palette", &json!({"main": "Red", "accent": 5}))
        .await
        .unwrap();
    assert_eq!(
        errors(&bad),
        vec![pair("body.main", "enum"), pair("body.accent", "enum")]
    );
    let null = c
        .post_json("/palette", &json!({"main": "dark-green", "accent": null}))
        .await
        .unwrap();
    assert_eq!(null.status, 200);
}

#[tokio::test]
async fn newtypes_delegate_and_apply_their_constraints() {
    let c = client();
    let bad = c
        .post_json("/holder", &json!({"wrapped": "x"}))
        .await
        .unwrap();
    assert_eq!(errors(&bad), vec![pair("body.wrapped", "too_short")]);
    let ok = c
        .post_json("/holder", &json!({"wrapped": "xy"}))
        .await
        .unwrap();
    assert_eq!(ok.status, 200);
}
