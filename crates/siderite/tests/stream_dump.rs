//! Streaming responses must preserve the existing value-dump contract.

use serde_json::{Value, json};
use siderite::prelude::*;
use siderite::validation::dump::DumpSerialize;
use siderite::validation::{Dump, DumpError, DumpOptions, FieldSet};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Serialize, Schema)]
struct Ordinary {
    #[serde(rename = "escaped\"key")]
    text: String,
    values: Vec<Option<String>>,
    #[field(exclude)]
    secret: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    absent: Option<u32>,
    float: f32,
}

#[derive(Serialize, Schema)]
#[schema(no_dump)]
struct Custom {
    value: u32,
}

impl Dump for Custom {
    fn dump(&self, _: &DumpOptions) -> Result<Value, DumpError> {
        Ok(json!({"custom": self.value + 1}))
    }
}

#[derive(Serialize, Schema)]
struct Nested {
    ordinary: Ordinary,
    custom: Vec<Option<Box<Custom>>>,
}

fn assert_equivalent<T: Dump>(value: &T, opts: &DumpOptions) -> TestResult {
    let expected = serde_json::to_vec(&value.dump(opts)?)?;
    let actual = serde_json::to_vec(&DumpSerialize(value, opts))?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn nested_streams_preserve_custom_dump_and_filters() -> TestResult {
    let value = Nested {
        ordinary: Ordinary {
            text: "quotes\" newline\n unicode λ".into(),
            values: vec![None, Some("one".into())],
            secret: "never on the wire".into(),
            absent: None,
            float: 0.1,
        },
        custom: vec![Some(Box::new(Custom { value: 7 })), None],
    };
    let fields = FieldSet::new().nested("ordinary", FieldSet::of(["values"]));
    for opts in [
        DumpOptions::default(),
        DumpOptions::new().exclude_none(),
        DumpOptions::new().exclude_defaults(),
        DumpOptions::new().include(FieldSet::of(["ordinary"])),
        DumpOptions::new().exclude(fields),
    ] {
        assert_equivalent(&value, &opts)?;
    }
    Ok(())
}

#[derive(Serialize, Schema)]
struct Hooked {
    value: u32,
}

impl siderite::validation::ModelHooks for Hooked {
    fn serialize_model(&self, _: Value) -> Result<Value, DumpError> {
        Ok(json!({"replacement": self.value}))
    }
}

#[derive(Serialize, Schema)]
struct HookContainer {
    nested: Vec<Hooked>,
}

#[test]
fn nested_hooks_use_the_value_path() -> TestResult {
    let value = HookContainer {
        nested: vec![Hooked { value: 9 }],
    };
    assert_equivalent(&value, &DumpOptions::default())?;
    let opts = DumpOptions::default();
    let actual = serde_json::to_value(DumpSerialize(&value, &opts))?;
    assert_eq!(actual, json!({"nested": [{"replacement": 9}]}));
    Ok(())
}

#[test]
fn numeric_edge_cases_keep_value_dump_semantics() -> TestResult {
    for value in [f32::NAN, f32::INFINITY, -0.0, 0.1, f32::MAX] {
        assert_equivalent(&value, &DumpOptions::default())?;
    }
    let opts = DumpOptions::default();
    assert!(serde_json::to_vec(&DumpSerialize(&u128::MAX, &opts)).is_err());
    assert!(serde_json::to_vec(&DumpSerialize(&i128::MIN, &opts)).is_err());
    Ok(())
}

#[derive(Serialize, Schema)]
#[schema(no_dump)]
struct Failing;

impl Dump for Failing {
    fn dump(&self, _: &DumpOptions) -> Result<Value, DumpError> {
        Err(serde_json::Error::io(std::io::Error::other("failed")).into())
    }
}

#[derive(Serialize, Schema)]
struct FailingContainer {
    first: String,
    last: Failing,
}

#[test]
fn failed_nested_dump_cannot_publish_partial_success() {
    let response = Json(FailingContainer {
        first: "partially encoded".into(),
        last: Failing,
    })
    .into_response();
    assert_eq!(response.status().as_u16(), 500);
}
