//! Liveness and readiness routes through the HTTP stack.
#![allow(clippy::unwrap_used)]

use siderite::health::{ReadinessChecks, liveness, readiness, sql_check};
use siderite::prelude::*;
use siderite_testkit::{TestClient, TestDatabase};

async fn db() -> Db {
    TestDatabase::sqlite_memory().await.unwrap().into_db()
}

fn app(checks: ReadinessChecks) -> App {
    App::new().routes([liveness("/livez"), readiness("/readyz", checks)])
}

#[tokio::test]
async fn ready_only_after_startup_and_while_checks_pass() {
    let check = sql_check(db().await);
    let checks = ReadinessChecks::new().check("database", check);
    // Without running startup the server is still Starting.
    let starting = TestClient::new(app(checks.clone()));
    let answer = starting.get("/readyz").await.unwrap();
    assert_eq!(answer.status, 503);
    let body: serde_json::Value = answer.json().unwrap();
    assert_eq!(
        body,
        serde_json::json!({"status": "unavailable", "checks": {}})
    );
    assert_eq!(starting.get("/livez").await.unwrap().status, 200);
    let running = TestClient::start(app(checks)).await.unwrap();
    let answer = running.get("/readyz").await.unwrap();
    assert_eq!(answer.status, 200);
    let cache = answer.headers.get("cache-control").unwrap();
    assert_eq!(cache, "no-store");
    let body: serde_json::Value = answer.json().unwrap();
    let checks = serde_json::json!({"database": "ok"});
    let want = serde_json::json!({"status": "ready", "checks": checks});
    assert_eq!(body, want);
    running.shutdown().await.unwrap();
}

#[tokio::test]
async fn failures_are_503_without_details() {
    let checks = ReadinessChecks::new()
        .check("database", sql_check(db().await))
        .check("cache", || async {
            Err("redis://admin:hunter2@cache:6379 refused".to_owned())
        });
    let running = TestClient::start(app(checks)).await.unwrap();
    let answer = running.get("/readyz").await.unwrap();
    assert_eq!(answer.status, 503);
    let text = answer.text();
    assert!(
        !text.contains("hunter2") && !text.contains("redis"),
        "{text}"
    );
    let body: serde_json::Value = answer.json().unwrap();
    assert_eq!(body["checks"]["cache"], "failed");
    assert_eq!(body["checks"]["database"], "ok");
    running.shutdown().await.unwrap();
}

#[tokio::test]
async fn both_routes_are_documented() {
    let document = app(ReadinessChecks::new()).openapi().unwrap();
    let value = serde_json::to_value(&document).unwrap();
    assert_eq!(value["paths"]["/readyz"]["get"]["operationId"], "readiness");
    assert_eq!(value["paths"]["/livez"]["get"]["operationId"], "liveness");
    assert!(value["components"]["schemas"]["ReadyReport"].is_object());
}
