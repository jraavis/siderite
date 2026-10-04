//! JSON extraction, validation and response serialization vs raw axum.
#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use axum::Router;
use axum::routing::post as axum_post;
use common::{axum_json_request, json_request};
use criterion::{Criterion, criterion_group, criterion_main};
use siderite::prelude::*;
use tower::ServiceExt;

/// Request and response model validated by siderite.
#[derive(Debug, Clone, Serialize, Deserialize, Validate, Schema)]
struct Item {
    #[field(min_length = 1, max_length = 100)]
    name: String,
    #[field(ge = 0, le = 1000)]
    quantity: i64,
    tags: Vec<String>,
    active: bool,
}

/// Same shape with plain serde, for the raw axum handlers.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawItem {
    name: String,
    quantity: i64,
    tags: Vec<String>,
    active: bool,
}

const BODY: &[u8] = br#"{"name":"widget","quantity":25,"tags":["a","b","c","d"],"active":true}"#;

async fn echo(Json(item): Json<Item>) -> Json<Item> {
    Json(item)
}

async fn raw_echo(axum::Json(item): axum::Json<RawItem>) -> axum::Json<RawItem> {
    axum::Json(item)
}

fn bench_extract(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let ours = App::new()
        .route("/items", post(echo))
        .into_router_service()
        .unwrap();
    let raw = Router::new().route("/items", axum_post(raw_echo));

    // Fail fast if a benchmark would measure an error path.
    let status = runtime
        .block_on(ours.clone().oneshot(json_request("/items", BODY)))
        .unwrap()
        .status();
    assert_eq!(status, http::StatusCode::OK);

    let mut group = c.benchmark_group("extract");
    group.bench_function("siderite/json_validate_respond", |b| {
        b.to_async(&runtime).iter(|| {
            let svc = ours.clone();
            async move { svc.oneshot(json_request("/items", BODY)).await.unwrap() }
        });
    });
    group.bench_function("axum/json_respond", |b| {
        b.to_async(&runtime).iter(|| {
            let svc = raw.clone();
            async move {
                svc.oneshot(axum_json_request("/items", BODY))
                    .await
                    .unwrap()
            }
        });
    });
    group.finish();

    let item: Item = serde_json::from_slice(BODY).unwrap();
    let mut group = c.benchmark_group("response_serialization");
    group.bench_function("siderite/dump", |b| {
        b.iter(|| Json(item.clone()).into_response());
    });
    group.bench_function("siderite/value_tree_reference", |b| {
        b.iter(|| value_tree_response(item.clone()));
    });
    group.bench_function("axum/response", |b| {
        b.iter(|| axum::response::IntoResponse::into_response(axum::Json(item.clone())));
    });
    group.bench_function("serde_json/to_vec", |b| {
        b.iter(|| serde_json::to_vec(&item).unwrap());
    });
    group.finish();
}

// Preserve the previous response algorithm as a same-run control.
fn value_tree_response(item: Item) -> siderite::Response {
    use siderite::validation::{Dump, DumpOptions};
    let value = item.dump(&DumpOptions::default()).unwrap();
    let bytes = serde_json::to_vec(&value).unwrap();
    let mut response = http::Response::new(siderite::Body::from(bytes));
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    response
}

criterion_group!(benches, bench_extract);
criterion_main!(benches);
