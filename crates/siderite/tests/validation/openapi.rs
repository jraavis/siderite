//! Validation metadata in the OpenAPI document.

use crate::fixtures::*;
use serde_json::Value;
use siderite::prelude::*;
use siderite::validation::{DumpOptions, ValidationContext, parse_value};

// ---------------------------------------------------------------------
// OpenAPI
// ---------------------------------------------------------------------

fn doc() -> Value {
    app().openapi().unwrap().to_value()
}

#[test]
fn computed_fields_are_read_only_properties() {
    let doc = doc();
    let user = &doc["components"]["schemas"]["User"];
    let props = &user["properties"];
    assert_eq!(props["fullName"]["readOnly"], true);
    assert_eq!(props["fullName"]["type"], "string");
    assert_eq!(props["fullName"]["description"], "Full display name.");
    assert_eq!(props["initials"]["readOnly"], true);
    // Computed fields are never required inputs.
    let required: Vec<&str> = user["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(!required.contains(&"fullName"));
    assert!(required.contains(&"firstName"));
    // Field constraints and defaults.
    assert_eq!(props["level"]["default"], 1);
}

#[test]
fn generic_computed_fields_appear_in_the_schema() {
    let mut registry = siderite::validation::SchemaRegistry::new();
    let inline = <Page<Item> as Schema>::schema(&mut registry).into_value();
    assert_eq!(inline["properties"]["count"]["readOnly"], true);
}

#[test]
fn extra_forbid_is_documented_and_the_document_is_valid_openapi_31() {
    let doc = doc();
    assert_eq!(
        doc["components"]["schemas"]["Signup"]["additionalProperties"],
        false
    );
    let meta: Value = serde_json::from_str(include_str!(
        "../../../siderite-openapi/tests/fixtures/openapi-3.1-schema.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&meta).unwrap();
    let problems: Vec<String> = validator.iter_errors(&doc).map(|e| e.to_string()).collect();
    assert!(problems.is_empty(), "{problems:#?}\n{doc:#}");
}

// ---------------------------------------------------------------------
// Key names agree between schema, prepare and dump
// ---------------------------------------------------------------------

#[test]
fn schema_prepare_and_dump_use_the_same_keys() {
    let schema = <Signup as Schema>::schema(&mut SchemaRegistry::new()).into_value();
    let mut keys: Vec<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["address", "age", "tags", "userName"]);

    let parsed: Signup = parse_value(valid_signup(), ValidationContext::new()).unwrap();
    let dumped = siderite::validation::Dump::dump(&parsed, &DumpOptions::new()).unwrap();
    let mut dumped_keys: Vec<&str> = dumped
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    dumped_keys.sort_unstable();
    assert_eq!(dumped_keys, keys);
}
