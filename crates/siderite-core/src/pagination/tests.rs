//! Page parameters, page conversion and their schemas.

#![allow(clippy::unwrap_used)]

use super::*;
use siderite_validation::{parse_value, text_pairs_to_value};

type Small = PageParams<5, 2>;

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
fn defaults_and_bounds() {
    let empty = query(&[]).unwrap();
    assert_eq!((empty.page(), empty.per_page()), (1, 2));
    let given = query(&[("page", "3"), ("per_page", "5")]).unwrap();
    assert_eq!((given.page(), given.per_page()), (3, 5));
    let other = query(&[("page", "2"), ("q", "ignored")]).unwrap();
    assert_eq!(other.page(), 2);
    for bad in [
        [("page", "0")],
        [("per_page", "0")],
        [("per_page", "6")],
        [("page", "x")],
        [("per_page", "-1")],
    ] {
        let errors = query(&bad).unwrap_err();
        assert_eq!(errors.len(), 1, "{bad:?}: {errors:?}");
        assert!(errors[0].contains(bad[0].0), "{errors:?}");
    }
    let default: PageParams = PageParams::default();
    assert_eq!(default.per_page(), 20);
}

#[test]
fn the_parameter_schema_states_the_bounds() {
    let mut registry = SchemaRegistry::default();
    let schema = Small::schema(&mut registry).into_value();
    assert_eq!(schema["properties"]["per_page"]["maximum"], 5);
    assert_eq!(schema["properties"]["per_page"]["default"], 2);
    assert_eq!(schema["properties"]["page"]["minimum"], 1);
}

fn orm_page() -> siderite_orm::Page<u64> {
    siderite_orm::Page {
        items: vec![1, 2],
        total: 5,
        page: 1,
        per_page: 2,
    }
}

#[test]
fn pages_convert_and_map() {
    let page: Page<u64> = orm_page().into();
    assert_eq!(page.total_pages, 3);
    let mapped = page.clone().map(|n| n * 10);
    assert_eq!(mapped.items, [10, 20]);
    assert_eq!((mapped.total, mapped.page), (5, 1));
    let failed = page
        .clone()
        .try_map(|n| if n == 2 { Err(n) } else { Ok(n) });
    assert_eq!(failed, Err(2));
    let dumped = page.dump(&DumpOptions::default()).unwrap();
    let want = json!({
        "items": [1, 2], "total": 5, "page": 1, "per_page": 2,
        "total_pages": 3,
    });
    assert_eq!(dumped, want);
    assert_eq!(serde_json::to_value(&page).unwrap(), want);
}

#[test]
fn page_schemas_are_inline_so_item_types_never_collide() {
    #[derive(Serialize)]
    struct A;
    impl Schema for A {
        fn schema_name() -> Option<&'static str> {
            Some("A")
        }
        fn schema(_: &mut SchemaRegistry) -> SchemaObject {
            SchemaObject::of_type("object")
        }
    }
    let mut registry = SchemaRegistry::default();
    assert_eq!(Page::<A>::schema_name(), None);
    let a = Page::<A>::schema(&mut registry).into_value();
    let n = Page::<u64>::schema(&mut registry).into_value();
    assert_eq!(
        a["properties"]["items"]["items"]["$ref"],
        "#/components/schemas/A"
    );
    assert_eq!(n["properties"]["items"]["items"]["type"], "integer");
    assert_eq!(a["required"].as_array().unwrap().len(), 5);
}
