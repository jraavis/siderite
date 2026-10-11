//! Breaking, additive and unknown changes between fixture documents.

#![allow(clippy::unwrap_used)]

use super::*;
use crate::fixtures::{ScratchDir, assert_schema_valid};
use serde_json::json;

/// A small API: list and create books, fetch one.
fn base() -> Value {
    json!({
        "openapi": "3.1.0",
        "info": { "title": "t", "version": "1" },
        "paths": {
            "/books": {
                "get": {
                    "parameters": [{
                        "name": "page", "in": "query",
                        "schema": { "type": "integer", "minimum": 1 }
                    }],
                    "responses": { "200": { "content": {
                        "application/json": { "schema": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/Book" }
                        } }
                    } } }
                },
                "post": {
                    "requestBody": { "required": true, "content": {
                        "application/json": { "schema":
                            { "$ref": "#/components/schemas/NewBook" } }
                    } },
                    "responses": { "201": { "content": {
                        "application/json": { "schema":
                            { "$ref": "#/components/schemas/Book" } }
                    } } }
                }
            },
            "/books/{id}": {
                "get": {
                    "parameters": [{
                        "name": "id", "in": "path", "required": true,
                        "schema": { "type": "integer" }
                    }],
                    "responses": {
                        "200": { "content": { "application/json": {
                            "schema": { "$ref": "#/components/schemas/Book" }
                        } } },
                        "404": { "description": "missing" }
                    }
                },
                "delete": { "responses": { "204": {} } }
            }
        },
        "components": { "schemas": {
            "Book": {
                "type": "object",
                "required": ["id", "title"],
                "properties": {
                    "id": { "type": "integer" },
                    "title": { "type": "string" },
                    "state": { "enum": ["draft", "published"] }
                }
            },
            "NewBook": {
                "type": "object",
                "additionalProperties": false,
                "required": ["title"],
                "properties": {
                    "title": { "type": "string", "maxLength": 200 },
                    "pages": { "type": ["integer", "null"] }
                }
            }
        } }
    })
}

fn kinds(changes: &[Change], impact: Impact) -> Vec<&'static str> {
    let found = changes.iter().filter(|c| c.impact == impact);
    found.map(|c| c.kind).collect()
}

/// Replace or insert the value at `pointer` (its parent must exist).
fn set(doc: &mut Value, pointer: &str, value: Value) {
    let (parent, key) = pointer.rsplit_once('/').unwrap();
    let key = key.replace("~1", "/").replace("~0", "~");
    let parent = doc.pointer_mut(parent).unwrap();
    parent.as_object_mut().unwrap().insert(key, value);
}

#[test]
fn identical_documents_have_no_changes() {
    assert!(compare(&base(), &base()).is_empty());
}

#[test]
fn breaking_changes_are_found_with_pointers() {
    let mut now = base();
    let paths = now["paths"].as_object_mut().unwrap();
    // Renamed parameter only: the same route.
    let item = paths.remove("/books/{id}").unwrap();
    paths.insert("/books/{key}".to_owned(), item);
    let book = "/components/schemas/Book";
    let new_book = "/components/schemas/NewBook";
    set(&mut now, &format!("{book}/required"), json!(["id"]));
    let props = format!("{new_book}/properties");
    set(&mut now, &format!("{props}/title/maxLength"), json!(100));
    set(&mut now, &format!("{props}/pages/type"), json!("integer"));
    set(
        &mut now,
        &format!("{new_book}/required"),
        json!(["title", "isbn"]),
    );
    set(
        &mut now,
        &format!("{props}/isbn"),
        json!({ "type": "string" }),
    );
    now["paths"]["/books/{key}"]
        .as_object_mut()
        .unwrap()
        .remove("delete");
    let changes = compare(&base(), &now);
    let breaking = kinds(&changes, Impact::Breaking);
    for kind in [
        "operation_removed",
        "property_no_longer_required",
        "constraint_tightened",
        "type_changed",
        "required_property_added",
    ] {
        assert!(breaking.contains(&kind), "{kind}: {changes:#?}");
    }
    let removed = changes.iter().find(|c| c.kind == "operation_removed");
    let removed = removed.unwrap();
    assert_eq!(removed.operation.as_deref(), Some("DELETE /books/{id}"));
    assert_eq!(removed.base.as_deref(), Some("/paths/~1books~1{id}/delete"));
    assert_eq!(removed.current, None);
    let kind = "required_property_added";
    let required = changes.iter().find(|c| c.kind == kind).unwrap();
    assert_eq!(
        required.current.as_deref(),
        Some("/components/schemas/NewBook/required")
    );
    // The same component is compared once per pair of locations, and the
    // routes are the same despite the parameter name.
    assert!(!breaking.contains(&"operation_added"));
    assert!(changes.windows(2).all(|w| w[0].impact <= w[1].impact));
}

#[test]
fn additive_changes_pass() {
    let mut now = base();
    let book = "/components/schemas/Book/properties";
    set(
        &mut now,
        &format!("{book}/isbn"),
        json!({ "type": "string" }),
    );
    let new_book = "/components/schemas/NewBook/properties";
    set(
        &mut now,
        &format!("{new_book}/subtitle"),
        json!({ "type": "string" }),
    );
    set(&mut now, &format!("{new_book}/title/maxLength"), json!(500));
    let get = &mut now["paths"]["/books"]["get"];
    get["parameters"].as_array_mut().unwrap().push(json!({
        "name": "q", "in": "query", "schema": { "type": "string" }
    }));
    get["responses"]["400"] = json!({ "description": "bad" });
    now["paths"]["/authors"] = json!({ "get": { "responses": {} } });
    let changes = compare(&base(), &now);
    assert!(
        changes.iter().all(|c| c.impact == Impact::Additive),
        "{changes:#?}"
    );
    let additive = kinds(&changes, Impact::Additive);
    for kind in [
        "property_added",
        "parameter_added",
        "response_added",
        "operation_added",
    ] {
        assert!(additive.contains(&kind), "{kind}: {changes:#?}");
    }
}

#[test]
fn requests_and_responses_are_directional() {
    let mut now = base();
    let state = "/components/schemas/Book/properties/state/enum";
    set(&mut now, state, json!(["draft", "published", "archived"]));
    let get = &mut now["paths"]["/books"]["get"]["parameters"][0];
    get["required"] = json!(true);
    let changes = compare(&base(), &now);
    // A new response value is undecidable; a newly required query
    // parameter breaks clients that omit it.
    assert_eq!(kinds(&changes, Impact::Unknown), ["enum_value_added"]);
    assert_eq!(
        kinds(&changes, Impact::Breaking),
        ["parameter_became_required"]
    );
}

#[test]
fn external_references_and_composed_schemas_are_unknown() {
    let mut now = base();
    let pointer = "/paths/~1books/post/requestBody/content/\
                   application~1json/schema";
    set(
        &mut now,
        pointer,
        json!({ "$ref": "https://example.com/book.json" }),
    );
    let book = "/components/schemas/Book";
    let variants = [json!({ "type": "object" }), json!({ "type": "null" })];
    let composed = json!({ "oneOf": variants });
    set(&mut now, book, composed);
    now["paths"]["/books"]["post"]["security"] = json!([{ "key": [] }]);
    let unknown = kinds(&compare(&base(), &now), Impact::Unknown);
    for kind in [
        "reference_not_resolved",
        "schema_not_compared",
        "not_compared",
    ] {
        assert!(unknown.contains(&kind), "{kind}: {unknown:?}");
    }
}

#[test]
fn the_command_reads_files_and_reports() {
    let dir = ScratchDir::new("openapi-diff");
    let (b, c) = (
        dir.path().join("base.json"),
        dir.path().join("current.json"),
    );
    std::fs::write(&b, base().to_string()).unwrap();
    let mut now = base();
    now["paths"].as_object_mut().unwrap().remove("/books/{id}");
    std::fs::write(&c, now.to_string()).unwrap();
    let words = ["openapi", "diff", "base.json", "current.json", "--json"];
    let raw: Vec<String> = words.iter().map(|s| (*s).to_owned()).collect();
    let options = parse(&raw, dir.path()).unwrap();
    assert!(options.json);
    let report = report(&options).unwrap();
    assert_eq!(report.exit_code(), 1);
    assert_eq!(report.data.summary.breaking, 2);
    assert_eq!(report.diagnostics[0].code(), "openapi.E002");
    assert_schema_valid(&serde_json::to_value(&report).unwrap());
    let text = render(&report);
    assert!(
        text.contains("GET /books/{id}: GET /books/{id} removed"),
        "{text}"
    );
    assert!(text.contains("2 breaking, 0 unknown, 0 additive"), "{text}");
    let same = Options {
        current: b.clone(),
        ..options.clone()
    };
    assert_eq!(super::report(&same).unwrap().exit_code(), 0);
    std::fs::write(&c, "{\"swagger\": \"2.0\"}").unwrap();
    let err = super::report(&options).unwrap_err();
    assert!(err.to_string().contains("not an OpenAPI 3"), "{err}");
    for bad in [
        &["openapi", "diff", "a.json"][..],
        &["openapi", "diff", "a", "b", "--x"],
    ] {
        let raw: Vec<String> = bad.iter().map(|s| (*s).to_owned()).collect();
        assert!(parse(&raw, dir.path()).is_err(), "{bad:?}");
    }
}

mod from_apps {
    use super::super::{Impact, compare};
    use siderite::prelude::*;

    #[derive(Debug, Deserialize, Validate, Schema)]
    struct NewNote {
        title: String,
    }

    #[derive(Debug, Deserialize, Validate, Schema)]
    #[schema(name = "NewNote")]
    struct NewNoteV2 {
        title: String,
        body: String,
    }

    #[derive(Debug, Serialize, Schema)]
    struct Note {
        id: i64,
        title: String,
    }

    #[derive(Debug, Serialize, Schema)]
    #[schema(name = "Note")]
    struct NoteV2 {
        id: i64,
        title: String,
        tags: Option<Vec<String>>,
    }

    async fn create(Json(n): Json<NewNote>) -> Json<Note> {
        Json(Note {
            id: 1,
            title: n.title,
        })
    }

    async fn create_v2(Json(n): Json<NewNoteV2>) -> Json<NoteV2> {
        let (title, tags) = (n.title, Some(vec![n.body]));
        Json(NoteV2 { id: 1, title, tags })
    }

    fn document(app: &App) -> serde_json::Value {
        serde_json::to_value(app.openapi().unwrap()).unwrap()
    }

    #[test]
    fn exported_documents_compare_through_their_references() {
        let v1 = App::new().route("/notes", post(create));
        let v2 = App::new().route("/notes", post(create_v2));
        let (old, new) = (document(&v1), document(&v2));
        assert!(compare(&old, &old).is_empty());
        let changes = compare(&old, &new);
        let breaking: Vec<_> = changes
            .iter()
            .filter(|c| c.impact == Impact::Breaking)
            .map(|c| (c.kind, c.current.clone().unwrap_or_default()))
            .collect();
        let want = "/components/schemas/NewNote/required";
        assert_eq!(breaking, [("required_property_added", want.to_owned())]);
        let added = changes.iter().filter(|c| c.impact == Impact::Additive);
        let added: Vec<_> = added.map(|c| c.kind).collect();
        assert!(added.contains(&"property_added"), "{changes:#?}");
        // Going back removes a response property clients may read.
        let back = compare(&new, &old);
        let kinds: Vec<_> = back.iter().map(|c| c.kind).collect();
        assert!(kinds.contains(&"property_removed"), "{back:#?}");
    }
}
