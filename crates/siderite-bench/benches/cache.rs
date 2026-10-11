//! Route cache: hit, refresh-fill and write-invalidation paths.
#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use common::empty_request;
use criterion::{Criterion, criterion_group, criterion_main};
use http::Method;
use siderite::cache::{MemoryCache, RouteCache};
use siderite::prelude::*;
use std::time::Duration;
use tower::ServiceExt;

async fn item() -> Cached<&'static str> {
    Cached::public(Duration::from_secs(60), "item")
}

async fn write() -> &'static str {
    "ok"
}

fn bench_cache(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let svc = App::new()
        .route("/item", get(item).post(write))
        .layer(RouteCache::new(MemoryCache::new(1024)))
        .into_router_service()
        .unwrap();
    let marker = |res: &http::Response<_>| res.headers()["x-cache"].to_str().unwrap().to_owned();
    // Warm and verify each benchmark measures the intended path.
    let warm = runtime
        .block_on(svc.clone().oneshot(empty_request(Method::GET, "/item")))
        .unwrap();
    assert_eq!(marker(&warm), "miss");
    let hit = runtime
        .block_on(svc.clone().oneshot(empty_request(Method::GET, "/item")))
        .unwrap();
    assert_eq!(marker(&hit), "hit");

    let mut group = c.benchmark_group("route_cache");
    group.bench_function("hit", |b| {
        b.to_async(&runtime).iter(|| {
            let svc = svc.clone();
            async move {
                let req = empty_request(Method::GET, "/item");
                svc.oneshot(req).await.unwrap()
            }
        });
    });
    group.bench_function("refresh_fill", |b| {
        b.to_async(&runtime).iter(|| {
            let svc = svc.clone();
            async move {
                let mut req = empty_request(Method::GET, "/item");
                req.headers_mut()
                    .insert("cache-control", "no-cache".parse().unwrap());
                svc.oneshot(req).await.unwrap()
            }
        });
    });
    group.bench_function("write_then_fill", |b| {
        b.to_async(&runtime).iter(|| {
            let svc = svc.clone();
            async move {
                let post = empty_request(Method::POST, "/item");
                svc.clone().oneshot(post).await.unwrap();
                let req = empty_request(Method::GET, "/item");
                svc.oneshot(req).await.unwrap()
            }
        });
    });
    group.finish();
}

criterion_group!(benches, bench_cache);
criterion_main!(benches);
