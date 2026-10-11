//! Constraint keys, config and edge cases.

use crate::fixtures::*;
use serde_json::{Value, json};
use siderite::prelude::*;
use siderite::validation::{DumpOptions, FieldError, ValidationContext, parse_value};

// ---------------------------------------------------------------------
// Constraint keys, config and edge cases
// ---------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[serde(deny_unknown_fields)]
struct Numbers {
    #[field(gt = 0, lt = 10)]
    open: i32,
    #[field(ge = -1.5, le = 2.5)]
    closed: f64,
    #[field(multiple_of = 5)]
    step: i64,
    #[field(multiple_of = 0.5)]
    half: f64,
    #[field(email, max_length = 20)]
    mail: String,
    #[field(url)]
    site: String,
    #[field(regex = "^[a-z]+$", max_digits = 5, decimal_places = 2)]
    slug: String,
    #[field(min_length = 1, max_length = 2)]
    list: Vec<u8>,
}

fn numbers() -> Value {
    json!({"open": 5, "closed": 0.5, "step": 10, "half": 1.5,
           "mail": "a@b.co", "site": "https://example.com/a", "slug": "abc", "list": [1]})
}

fn codes_of<T: serde::de::DeserializeOwned + Validate>(input: Value) -> Vec<(String, String)> {
    match parse_value::<T>(input, ValidationContext::new()) {
        Ok(_) => Vec::new(),
        Err(e) => e
            .errors
            .into_iter()
            .map(|e| {
                let loc: Vec<String> = e.location.iter().map(ToString::to_string).collect();
                (loc.join("."), e.code.into_owned())
            })
            .collect(),
    }
}

#[test]
fn numeric_string_and_collection_constraints() {
    assert!(codes_of::<Numbers>(numbers()).is_empty());
    let mut bad = numbers();
    bad["open"] = json!(10);
    bad["closed"] = json!(-2);
    bad["step"] = json!(7);
    bad["half"] = json!(1.2);
    bad["mail"] = json!("nope");
    bad["site"] = json!("not a url");
    bad["slug"] = json!("ABC");
    bad["list"] = json!([1, 2, 3]);
    assert_eq!(
        codes_of::<Numbers>(bad),
        vec![
            pair("open", "less_than"),
            pair("closed", "greater_than_equal"),
            pair("step", "multiple_of"),
            pair("half", "multiple_of"),
            pair("mail", "invalid_email"),
            pair("site", "url_parsing"),
            pair("slug", "pattern_mismatch"),
            pair("list", "too_long"),
        ]
    );
}

#[test]
fn deny_unknown_fields_means_extra_forbid() {
    let mut input = numbers();
    input["surprise"] = json!(1);
    assert_eq!(
        codes_of::<Numbers>(input),
        vec![pair("surprise", "extra_forbidden")]
    );
    let schema = <Numbers as Schema>::schema(&mut SchemaRegistry::new()).into_value();
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["slug"]["pattern"], "^[a-z]+$");
    assert_eq!(schema["properties"]["slug"]["x-max-digits"], 5);
    assert_eq!(schema["properties"]["half"]["multipleOf"], 0.5);
    assert_eq!(schema["properties"]["mail"]["format"], "email");
}

#[test]
fn two_names_for_one_field_are_a_duplicate() {
    // `populate_by_name`: the key, the validation alias and the Rust name all
    // reach `user_name`; the first one present wins, the rest are rejected.
    let mut input = valid_signup();
    input["login"] = json!("eve");
    input["user_name"] = json!("amy");
    assert_eq!(
        codes_of::<Signup>(input),
        vec![
            pair("login", "duplicate_field"),
            pair("user_name", "duplicate_field"),
        ]
    );
}

#[derive(Debug, Deserialize, Validate, Schema)]
#[model_config(extra = "forbid")]
struct ServerStamped {
    name: String,
    #[serde(skip_deserializing)]
    stamp: u8,
}

#[test]
fn a_skip_deserializing_key_is_extra_under_forbid() {
    // Like Serde's `deny_unknown_fields`: the field is not an input, so its
    // key is an unknown one.
    assert!(codes_of::<ServerStamped>(json!({"name": "a"})).is_empty());
    assert_eq!(
        codes_of::<ServerStamped>(json!({"name": "a", "stamp": 1})),
        vec![pair("stamp", "extra_forbidden")]
    );
}

#[derive(Debug, Deserialize, Validate, Schema)]
#[model_config(extra = "allow")]
struct Permissive {
    a: u8,
}

#[derive(Debug, Deserialize, Validate, Schema)]
#[model_config(hooks)]
struct Direct {
    a: u8,
}

#[model_hooks]
impl Direct {
    #[model_validator]
    fn nonzero(&self) -> Result<(), FieldError> {
        if self.a == 0 {
            Err(FieldError::new("zero", "a must not be zero"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn extra_allow_and_direct_hooks_on_concrete_types() {
    let ok: Permissive = parse_value(json!({"a": 1, "x": 2}), ValidationContext::new()).unwrap();
    assert_eq!(ok.a, 1);
    assert_eq!(codes_of::<Direct>(json!({"a": 0})), vec![pair("", "zero")]);
    assert!(codes_of::<Direct>(json!({"a": 3})).is_empty());
}

/// Input-only types may derive `Schema` without `Serialize`, or opt out of
/// `Dump` explicitly.
#[derive(Debug, Deserialize, Validate, Schema)]
struct InputOnly {
    a: u8,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[schema(no_dump)]
struct OptedOut {
    a: u8,
}

fn assert_dump<T: siderite::validation::Dump>() {}

#[test]
fn dump_applies_exactly_to_serializable_types() {
    assert_dump::<Signup>();
    assert_dump::<Page<Item>>();
    assert_dump::<Wrapper>();
    assert_dump::<Color>();
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Nested {
    inner: Option<Wrapper>,
    list: Vec<Wrapper>,
}

#[test]
fn newtype_dump_delegates_and_nested_options_hold() {
    let n = Nested {
        inner: None,
        list: vec![Wrapper("ab".into())],
    };
    let dumped = siderite::validation::Dump::dump(&n, &DumpOptions::new().exclude_none()).unwrap();
    assert_eq!(dumped, json!({"list": ["ab"]}));
}

/// Generic models without `hooks` still validate their items.
#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Bag<T> {
    items: Vec<T>,
}

#[test]
fn generic_models_without_hooks_validate_items() {
    let got = codes_of::<Bag<Item>>(json!({"items": [{"qty": 0}, {"qty": 2}]}));
    assert_eq!(got, vec![pair("items.0.qty", "greater_than_equal")]);
    assert_dump::<Bag<Item>>();
}
