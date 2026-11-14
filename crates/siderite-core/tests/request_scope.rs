//! What every routed request carries — registered state, the DI scope and
//! global dependencies — on endpoints, 405 answers, mounts and services.
#![allow(clippy::unwrap_used)]

use siderite_core::di::{Dependency, Depends, Provided, ResolveContext};
use siderite_core::http::StatusCode;
use siderite_core::*;
use siderite_testkit::TestClient;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Debug)]
struct Label(&'static str);

async fn label(State(label): State<Label>) -> String {
    label.0.to_owned()
}

#[tokio::test]
async fn first_registered_state_wins() {
    let app = App::new()
        .with_state(Label("first"))
        .with_state(Label("second"))
        .route("/", get(label));
    let res = TestClient::new(app).get("/").await.unwrap();
    assert_eq!(res.text(), "first");
}

#[tokio::test]
async fn child_state_shadows_parent_state_inside_the_child() {
    let child = App::new()
        .with_state(Label("child"))
        .route("/l", get(label));
    let app = App::new()
        .with_state(Label("parent"))
        .route("/l", get(label))
        .mount("/c", child);
    let client = TestClient::new(app);
    assert_eq!(client.get("/l").await.unwrap().text(), "parent");
    assert_eq!(client.get("/c/l").await.unwrap().text(), "child");
}

#[tokio::test]
async fn parent_state_reaches_a_child_without_its_own() {
    let child = App::new().route("/l", get(label));
    let app = App::new().with_state(Label("parent")).mount("/c", child);
    let res = TestClient::new(app).get("/c/l").await.unwrap();
    assert_eq!(res.text(), "parent");
}

struct Auth;

impl Dependency for Auth {
    async fn resolve(ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        match ctx.parts().headers.get("x-token") {
            Some(token) if token == "secret" => Ok(Auth),
            _ => Err(ApiError::new(StatusCode::UNAUTHORIZED, "bad token")),
        }
    }
}

#[tokio::test]
async fn global_dependency_also_guards_method_not_allowed() {
    let app = App::new()
        .route("/a", get(|| async { "a" }))
        .dependency::<Auth>();
    let client = TestClient::new(app);
    let req = ::http::Request::post("/a").body(Body::empty()).unwrap();
    let res = client.send(req).await.unwrap();
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    let req = ::http::Request::post("/a")
        .header("x-token", "secret")
        .body(Body::empty())
        .unwrap();
    let res = client.send(req).await.unwrap();
    assert_eq!(res.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(res.headers.get("allow").unwrap(), "GET,HEAD");
}

#[tokio::test]
async fn method_not_allowed_keeps_the_allow_header() {
    let app = App::new().route("/a", get(|| async { "a" }).delete(|| async { "d" }));
    let req = ::http::Request::put("/a").body(Body::empty()).unwrap();
    let res = TestClient::new(app).send(req).await.unwrap();
    assert_eq!(res.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(res.headers.get("allow").unwrap(), "GET,HEAD,DELETE");
}

#[tokio::test]
async fn child_global_dependency_runs_only_for_the_child() {
    let child = App::new()
        .route("/c", get(|| async { "c" }))
        .dependency::<Auth>();
    let app = App::new()
        .route("/a", get(|| async { "a" }))
        .mount("/m", child);
    let client = TestClient::new(app);
    assert_eq!(client.get("/a").await.unwrap().status, StatusCode::OK);
    let res = client.get("/m/c").await.unwrap();
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.content_type(), Some("application/problem+json"));
}

#[derive(Default)]
struct Probe {
    resolves: AtomicUsize,
    teardowns: Mutex<Vec<&'static str>>,
    done: Notify,
}

struct Counted;

impl Dependency for Counted {
    async fn resolve(ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        let probe = Provided::<Probe>::from_request_parts(ctx.parts()).await?.0;
        probe.resolves.fetch_add(1, Ordering::SeqCst);
        ctx.on_teardown(async move {
            probe.teardowns.lock().unwrap().push("counted");
            probe.done.notify_one();
        });
        Ok(Counted)
    }
}

async fn counted(_a: Depends<Counted>, _b: Depends<Counted>) -> &'static str {
    "ok"
}

#[tokio::test]
async fn mounted_endpoint_shares_one_scope_and_tears_down_once() {
    let probe = Arc::new(Probe::default());
    // The parent's global resolves `Counted` before the child's handler,
    // which must reuse it from the same request scope.
    let child = App::new().route("/t", get(counted));
    let app = App::new()
        .provide_arc(Arc::clone(&probe))
        .dependency::<Counted>()
        .mount("/m", child);
    let res = TestClient::new(app).get("/m/t").await.unwrap();
    assert_eq!(res.text(), "ok");
    tokio::time::timeout(Duration::from_secs(5), probe.done.notified())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(probe.resolves.load(Ordering::SeqCst), 1);
    assert_eq!(*probe.teardowns.lock().unwrap(), ["counted"]);
}

struct Missing;

async fn needs_missing(_m: Provided<Missing>) -> &'static str {
    "unreachable"
}

#[tokio::test]
async fn provided_lookup_misses_in_a_mount_are_a_500_problem() {
    let child = App::new().provide(3_u8).route("/x", get(needs_missing));
    let app = App::new().provide(7_u16).mount("/m", child);
    let res = TestClient::new(app).get("/m/x").await.unwrap();
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(res.content_type(), Some("application/problem+json"));
}

async fn both(a: Provided<u8>, b: Provided<u16>) -> String {
    format!("{}/{}", a.0, b.0)
}

#[tokio::test]
async fn provided_values_resolve_through_the_mount_chain() {
    let child = App::new().provide(3_u8).route("/x", get(both));
    let app = App::new().provide(1_u8).provide(7_u16).mount("/m", child);
    let res = TestClient::new(app).get("/m/x").await.unwrap();
    assert_eq!(res.text(), "3/7");
}
