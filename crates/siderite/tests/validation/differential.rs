//! Differential checks of `prepare` against Serde.

use crate::fixtures::*;
use serde_json::{Value, json};
use siderite::validation::model::prepare_agrees_with_serde;

// ---------------------------------------------------------------------
// Differential: prepare vs Serde
// ---------------------------------------------------------------------

fn common_inputs() -> Vec<Value> {
    vec![
        json!({}),
        json!(null),
        json!([]),
        json!(5),
        json!("text"),
        json!({"extra": 1}),
        json!({"n": 1}),
        json!({"n": "1"}),
        json!({"n": 1.5}),
        json!({"n": null}),
        json!({"n": true}),
        json!({"n": [1]}),
        json!({"n": {"x": 1}}),
        json!({"n": 3000000000_i64}),
        json!({"n": -3000000000_i64}),
        json!({"n": "abc"}),
        json!({"n": 2.0}),
    ]
}

fn inputs_for(specific: Vec<Value>) -> Vec<Value> {
    let mut all = common_inputs();
    all.extend(specific);
    all
}

#[test]
fn signup_prepare_agrees_with_serde() {
    let good_address = json!({"zip": "12345", "city": "c"});
    let inputs = inputs_for(vec![
        json!({}),
        json!({"userName": "ab", "tags": ["x"], "address": good_address}),
        json!({"userName": 5, "tags": "x", "address": good_address}),
        json!({"userName": "ab", "tags": [1], "address": good_address}),
        json!({"login": "ab", "userName": "cd", "tags": [], "address": good_address}),
        json!({"userName": "ab", "age": 300, "tags": [], "address": good_address}),
        json!({"userName": "ab", "age": null, "tags": [], "address": good_address}),
        json!({"userName": "ab", "age": "19", "tags": [], "address": good_address}),
        json!({"userName": "ab", "age": -1, "tags": [], "address": good_address}),
        json!({"user_name": "ab", "tags": [], "address": good_address}),
        json!({"userName": "ab", "tags": [], "address": null}),
        json!({"userName": "ab", "tags": [], "address": {"zip": 1, "city": "c"}}),
        json!({"userName": "ab", "tags": [], "address": {"zip": "1", "city": null}}),
        json!({"userName": "ab", "tags": [], "address": [], "extra": true}),
        json!({"userName": "  ab  ", "tags": ["a", null], "address": good_address}),
    ]);
    assert!(inputs.len() >= 15);
    prepare_agrees_with_serde::<Signup>(&inputs).unwrap();
}

#[test]
fn line_and_basket_prepare_agrees_with_serde() {
    let inputs = inputs_for(vec![
        json!({"qty": 1, "code": "a"}),
        json!({"qty": "1", "code": "a"}),
        json!({"qty": 0, "code": "a"}),
        json!({"qty": 11, "code": "a"}),
        json!({"qty": -1, "code": "a"}),
        json!({"qty": 1.0, "code": "a"}),
        json!({"qty": 1.5, "code": "a"}),
        json!({"qty": 1, "code": ""}),
        json!({"qty": 1, "code": "abcd"}),
        json!({"qty": 1, "code": 5}),
        json!({"qty": 1}),
        json!({"code": "a"}),
        json!({"qty": null, "code": null}),
        json!({"qty": 4294967296_u64, "code": "a"}),
        json!({"qty": 1, "code": "a", "more": {"deep": [1, 2]}}),
    ]);
    prepare_agrees_with_serde::<Line>(&inputs).unwrap();
    let baskets = inputs_for(vec![
        json!({"items": []}),
        json!({"items": [{"qty": 1, "code": "a"}]}),
        json!({"items": [{"qty": 1, "code": "a"}, {"qty": "x"}]}),
        json!({"items": {}}),
        json!({"items": null}),
        json!({"items": [null]}),
        json!({"items": [[]]}),
        json!({"items": "one"}),
        json!({"items": [{"qty": 2, "code": "b"}, {"qty": 2, "code": "b"}]}),
        json!({"items": [1, 2, 3]}),
        json!({"items": [{}]}),
        json!({"item": []}),
        json!({"items": [{"qty": 1, "code": "a"}], "x": 1}),
    ]);
    prepare_agrees_with_serde::<Basket>(&baskets).unwrap();
}

#[test]
fn search_and_palette_prepare_agrees_with_serde() {
    let inputs = inputs_for(vec![
        json!({"q": "x"}),
        json!({"q": "x", "limit": 5}),
        json!({"q": "x", "limit": "5"}),
        json!({"q": "x", "limit": -5}),
        json!({"q": "x", "limit": 500}),
        json!({"q": "x", "tags": ["a"]}),
        json!({"q": "x", "tags": "a"}),
        json!({"q": "x", "tags": null}),
        json!({"q": "x", "active": "yes"}),
        json!({"q": "x", "active": 2}),
        json!({"q": "x", "active": 1}),
        json!({"q": 1}),
        json!({"limit": 1}),
        json!({"q": "x", "limit": null}),
        json!({"q": "x", "active": null}),
    ]);
    prepare_agrees_with_serde::<Search>(&inputs).unwrap();
    let palettes = inputs_for(vec![
        json!({"main": "red"}),
        json!({"main": "green"}),
        json!({"main": "dark-green"}),
        json!({"main": "blue"}),
        json!({"main": "Red"}),
        json!({"main": 1}),
        json!({"main": null}),
        json!({"main": "red", "accent": null}),
        json!({"main": "red", "accent": "nope"}),
        json!({"main": "red", "accent": []}),
        json!({"accent": "red"}),
        json!({"main": ["red"]}),
        json!({"main": "red", "extra": 1}),
    ]);
    prepare_agrees_with_serde::<Palette>(&palettes).unwrap();
}
