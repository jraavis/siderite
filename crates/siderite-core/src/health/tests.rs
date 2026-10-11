//! Liveness answers in-process in every phase and without databases.

use super::*;
use crate::App;
use http::{Request, StatusCode, header};
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn answers_alive_without_readiness_or_databases() -> TestResult {
    let app = App::new().routes([liveness("/livez")]);
    // Built without running startup: the readiness phase stays Starting.
    let service = app.into_router_service()?;
    let request = Request::get("/livez").body(crate::Body::empty())?;
    let response = service.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let cache = response.headers().get(header::CACHE_CONTROL);
    assert_eq!(cache.map(|v| v.as_bytes()), Some(&b"no-store"[..]));
    let body = response.into_body().into_bytes().await?;
    assert_eq!(&body[..], br#"{"status":"alive"}"#);
    Ok(())
}

#[test]
fn it_is_documented_and_never_registered_implicitly() -> TestResult {
    let empty = App::new().openapi()?;
    let text = serde_json::to_string(&empty)?;
    assert!(!text.contains("liveness"), "nothing without opting in");
    let document = App::new().routes([liveness("/livez")]).openapi()?;
    let value = serde_json::to_value(&document)?;
    let operation = &value["paths"]["/livez"]["get"];
    assert_eq!(operation["operationId"], "liveness");
    assert_eq!(operation["tags"][0], "health");
    let alive = &value["components"]["schemas"]["Alive"];
    assert_eq!(alive["properties"]["status"]["const"], "alive");
    Ok(())
}

#[test]
fn a_colliding_path_is_rejected_when_the_app_is_built() {
    let app = App::new()
        .route("/livez", get(|| async { "mine" }))
        .routes([liveness("/livez")]);
    assert!(app.into_router_service().is_err());
}
