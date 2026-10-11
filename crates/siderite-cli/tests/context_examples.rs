//! The examples `ai init` puts in `.siderite/context.md` compile against
//! this checkout and behave as their comments say.

#![allow(clippy::unwrap_used)]

mod route {
    include!("../src/ai/examples/route.rs");
}

mod model {
    include!("../src/ai/examples/model.rs");
}

mod main_example {
    include!("../src/ai/examples/main.rs");
}

use siderite::prelude::*;
use siderite_testkit::TestClient;

#[tokio::test]
async fn the_route_example_serves_and_validates() {
    let client = TestClient::new(route::app());
    let found = client.get("/notes/1").await.unwrap();
    assert_eq!(found.status, 200);
    let missing = client.get("/notes/2").await.unwrap();
    assert_eq!(missing.status, 404);
    let body = serde_json::json!({ "text": "" });
    let empty = client.post_json("/notes", &body).await.unwrap();
    assert_eq!(empty.status, 422);
    let body = serde_json::json!({ "text": "buy milk" });
    let created = client.post_json("/notes", &body).await.unwrap();
    assert_eq!(created.status, 201);
}

#[test]
fn the_model_and_command_line_examples_compile() {
    assert_eq!(model::Product::META.table, "products");
    let app = App::new().routes(routes![model::list_products]);
    assert!(app.openapi().is_ok());
    // `main` is the whole binary; referencing it proves it type-checks.
    let _: fn() -> std::process::ExitCode = main_example::main;
}
