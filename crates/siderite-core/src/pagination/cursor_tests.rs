//! Cursor parameters, page conversion and their schemas.

#![allow(clippy::unwrap_used)]

use super::cursor::*;
use serde_json::json;
use siderite_validation::{Dump, DumpOptions, Schema, SchemaRegistry};
use siderite_validation::{ValidationContext, parse_value, text_pairs_to_value};

type Small = CursorParams<5, 2>;

fn query(pairs: &[(&str, &str)]) -> Result<Small, Vec<String>> {
    let input = text_pairs_to_value(pairs.iter().copied());
    let parsed = parse_value::<Small>(input, ValidationContext::for_text());
    parsed.map_err(|errors| {
        let found = errors.errors.iter();
        found
            .map(|e| format!("{:?}:{}", e.location, e.code))
            .collect()
    })
}

#[test]
fn defaults_tokens_and_bounds() {
    let empty = query(&[]).unwrap();
    assert_eq!(empty.limit(), 2);
    assert_eq!((empty.after, empty.before), (None, None));
    let after = query(&[("after", "abc"), ("limit", "5")]).unwrap();
    assert_eq!(after.after.as_deref(), Some("abc"));
    assert_eq!(after.limit(), 5);
    let before = query(&[("before", "xyz"), ("q", "ignored")]).unwrap();
    assert_eq!(before.before.as_deref(), Some("xyz"));
    for bad in [
        [("limit", "0")],
        [("limit", "6")],
        [("limit", "x")],
        [("limit", "-1")],
    ] {
        let errors = query(&bad).unwrap_err();
        assert_eq!(errors.len(), 1, "{bad:?}: {errors:?}");
        assert!(errors[0].contains("limit"), "{errors:?}");
    }
    let default: CursorParams = CursorParams::default();
    assert_eq!(default.limit(), 20);
}

#[test]
fn both_cursors_are_rejected_with_422() {
    let errors = query(&[("after", "a"), ("before", "b")]).unwrap_err();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("before") && errors[0].contains("exclusive"));
}

#[test]
fn the_parameter_schema_states_the_bounds() {
    let mut registry = SchemaRegistry::default();
    let schema = Small::schema(&mut registry).into_value();
    assert_eq!(schema["properties"]["limit"]["maximum"], 5);
    assert_eq!(schema["properties"]["limit"]["default"], 2);
    assert_eq!(schema["properties"]["limit"]["minimum"], 1);
    assert_eq!(schema["properties"]["after"]["type"], "string");
    assert_eq!(schema["properties"]["before"]["type"], "string");
}

fn orm_page() -> siderite_orm::CursorPage<u64> {
    siderite_orm::CursorPage {
        items: vec![1, 2],
        next: Some("n".into()),
        previous: None,
    }
}

#[test]
fn pages_convert_and_map() {
    let page: CursorPage<u64> = orm_page().into();
    let mapped = page.clone().map(|n| n * 10);
    assert_eq!(mapped.items, [10, 20]);
    assert_eq!(mapped.next.as_deref(), Some("n"));
    let failed = page
        .clone()
        .try_map(|n| if n == 2 { Err(n) } else { Ok(n) });
    assert_eq!(failed, Err(2));
    let dumped = page.dump(&DumpOptions::default()).unwrap();
    let want = json!({"items": [1, 2], "next": "n", "previous": null});
    assert_eq!(dumped, want);
    assert_eq!(serde_json::to_value(&page).unwrap(), want);
}

#[test]
fn page_schemas_are_inline_and_have_no_total() {
    let mut registry = SchemaRegistry::default();
    assert_eq!(CursorPage::<u64>::schema_name(), None);
    let schema = CursorPage::<u64>::schema(&mut registry).into_value();
    let properties = schema["properties"].as_object().unwrap();
    let names: Vec<_> = properties.keys().map(String::as_str).collect();
    assert!(names.contains(&"items") && !names.contains(&"total"));
    assert_eq!(schema["properties"]["items"]["items"]["type"], "integer");
    let next = &schema["properties"]["next"];
    assert_eq!(next["anyOf"][1]["type"], "null");
    assert_eq!(schema["required"].as_array().unwrap().len(), 3);
}
