//! The `api` template of `siderite new`: a documented, tested endpoint, a
//! commented configuration and the developer workflow in the README.
//! Generated projects pass `siderite verify` (checked by a slow test).

/// `src/lib.rs`: a validated endpoint and a liveness route.
pub(super) fn lib_rs(ident: &str) -> String {
    format!(
        "\
//! {ident} HTTP API.

use siderite::health::liveness;
use siderite::prelude::*;

/// Body of `POST /greetings`; invalid input is answered with 422.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct NewGreeting {{
    /// Who to greet.
    #[field(min_length = 1, max_length = 50)]
    pub name: String,
}}

/// A greeting.
#[derive(Debug, Serialize, Schema)]
pub struct Greeting {{
    /// The greeting text.
    pub message: String,
}}

/// Greet someone.
#[post(\"/greetings\", status = 201, tag = \"greetings\")]
pub async fn create_greeting(
    Json(body): Json<NewGreeting>,
) -> Json<Greeting> {{
    let message = format!(\"Hello, {{}}!\", body.name);
    Json(Greeting {{ message }})
}}

/// The application: every route is registered explicitly.
pub fn app() -> App {{
    App::new()
        .title(\"{ident}\")
        .version(\"0.1.0\")
        .routes(routes![create_greeting])
        .routes([liveness(\"/livez\")])
}}
"
    )
}

/// `tests/api.rs`: the endpoint's success and failure cases.
pub(super) fn tests_rs(ident: &str) -> String {
    format!(
        "\
//! The API through the in-process test client.

use siderite_testkit::TestClient;

#[tokio::test]
async fn greets_valid_names() {{
    let client = TestClient::new({ident}::app());
    let body = serde_json::json!({{ \"name\": \"Ada\" }});
    let response = client.post_json(\"/greetings\", &body).await.unwrap();
    assert_eq!(response.status, 201);
    let greeting: serde_json::Value = response.json().unwrap();
    assert_eq!(greeting[\"message\"], \"Hello, Ada!\");
}}

#[tokio::test]
async fn rejects_invalid_input() {{
    let client = TestClient::new({ident}::app());
    for body in [
        serde_json::json!({{ \"name\": \"\" }}),
        serde_json::json!({{ \"nom\": \"Ada\" }}),
        serde_json::json!({{ \"name\": 42 }}),
    ] {{
        let response = client.post_json(\"/greetings\", &body).await.unwrap();
        assert_eq!(response.status, 422, \"{{body}}\");
    }}
}}

#[tokio::test]
async fn reports_liveness_and_documents_the_api() {{
    let app = {ident}::app();
    let document = serde_json::to_value(app.openapi().unwrap()).unwrap();
    assert!(document[\"paths\"][\"/greetings\"][\"post\"].is_object());
    let client = TestClient::new(app);
    assert_eq!(client.get(\"/livez\").await.unwrap().status, 200);
}}
"
    )
}

/// `siderite.toml` with the main settings explained.
pub(super) fn siderite_toml(name: &str, url: &str) -> String {
    format!(
        "\
# Settings for {name}. Environment variables override this file:
# SIDERITE_SERVER__ADDR, SIDERITE_DATABASES__DEFAULT__URL, ...
# (DATABASE_URL also sets the default database).

[app]
name = \"{name}\"
debug = false

[server]
# Where `siderite run` listens.
addr = \"127.0.0.1:8000\"

[databases.default]
# Models and migrations use this database.
url = \"{url}\"
# max_connections = 10

[log]
level = \"info\"
json = false
"
    )
}

/// `README.md`: how to work on the project.
pub(super) fn readme(name: &str) -> String {
    format!(
        "\
# {name}

An HTTP API built with siderite.

## Develop

```bash
siderite dev             # rebuild and restart on every change
siderite verify          # fmt, lint, build, test and framework checks
siderite test            # the tests in tests/
```

Open http://127.0.0.1:8000/docs for the API documentation and
http://127.0.0.1:8000/livez for the liveness check.

## Grow it

```bash
siderite generate resource book title:String --table books
siderite makemigrations  # after adding or changing models
siderite migrate
siderite add --preset postgres   # switch the backend support
```

Settings live in `siderite.toml`; environment variables override them.
`siderite ai context --task \"...\"` prepares context for AI tools, and
`siderite explain CODE` explains any diagnostic code.
"
    )
}
