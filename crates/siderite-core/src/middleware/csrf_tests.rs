//! Origin checks, driven through a real application.

use super::{CsrfOrigin, CsrfOriginError, parse_origin};
use crate::app::App;
use crate::body::Body;
use crate::routing::{delete, get, post, put};
use http::{Method, Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

const APP: &str = "https://app.example.com";

fn csrf() -> CsrfOrigin {
    CsrfOrigin::new([APP, "http://localhost:3000"]).unwrap()
}

async fn send(
    csrf: CsrfOrigin,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, Vec<u8>) {
    let app = App::new()
        .route("/x", get(|| async { "ok" }).post(|| async { "ok" }))
        .route("/y", put(|| async { "ok" }).patch(|| async { "ok" }))
        .route("/z", delete(|| async { "ok" }))
        .route("/hooks/a", post(|| async { "ok" }))
        .route("/hooksy", post(|| async { "ok" }))
        .layer(csrf);
    let mut req = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let service = app.into_router_service().unwrap();
    let res = service.oneshot(req.body(Body::empty()).unwrap()).await;
    let res = res.unwrap();
    let status = res.status();
    let body = res.into_body().into_bytes().await.unwrap();
    (status, body)
}

async fn status(method: Method, headers: &[(&str, &str)]) -> StatusCode {
    send(csrf(), method, "/x", headers).await.0
}

#[test]
fn origins_are_normalized() {
    let same = [
        "https://app.example.com",
        "HTTPS://App.Example.COM",
        "https://app.example.com:443",
    ];
    for raw in same {
        assert_eq!(parse_origin(raw).as_deref(), Some(APP), "{raw}");
    }
    assert_eq!(
        parse_origin("http://a.test:80").as_deref(),
        Some("http://a.test")
    );
    assert_eq!(
        parse_origin("https://a.test:8443").as_deref(),
        Some("https://a.test:8443")
    );
    assert_eq!(
        parse_origin("http://[::1]:3000").as_deref(),
        Some("http://[::1]:3000")
    );
}

#[test]
fn invalid_origins_are_construction_errors() {
    let bad = [
        "",
        "app.example.com",
        "ftp://app.example.com",
        "https://",
        "https://app.example.com/",
        "https://app.example.com/path",
        "https://app.example.com?x=1",
        "https://user@app.example.com",
        "https://app.example.com:",
        "https://app.example.com:0",
        "https://app.example.com:99999",
        "https://app .example.com",
        "null",
        "*",
    ];
    for raw in bad {
        let error = CsrfOrigin::new([raw]).unwrap_err();
        assert_eq!(error, CsrfOriginError(raw.to_owned()), "{raw}");
        assert!(error.to_string().contains("invalid CSRF origin"));
    }
    assert!(CsrfOrigin::new([APP, "nope"]).is_err());
}

#[tokio::test]
async fn safe_methods_always_pass() {
    for method in [Method::GET, Method::HEAD, Method::OPTIONS, Method::TRACE] {
        let hostile = [("origin", "https://evil.test")];
        let (status, _) = send(csrf(), method, "/x", &hostile).await;
        assert_ne!(status, StatusCode::FORBIDDEN);
    }
}

#[tokio::test]
async fn every_unsafe_method_is_checked() {
    let cases = [
        (Method::POST, "/x"),
        (Method::PUT, "/y"),
        (Method::PATCH, "/y"),
        (Method::DELETE, "/z"),
        (Method::from_bytes(b"PURGE").unwrap(), "/x"),
    ];
    for (method, path) in cases {
        let good = [("origin", APP)];
        let (status, _) = send(csrf(), method.clone(), path, &good).await;
        assert_ne!(status, StatusCode::FORBIDDEN, "{method}");
        let (status, _) = send(csrf(), method.clone(), path, &[]).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} no origin");
        let evil = [("origin", "https://evil.test")];
        let (status, _) = send(csrf(), method.clone(), path, &evil).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} foreign");
    }
    assert_eq!(status(Method::POST, &[("origin", APP)]).await, 200);
}

#[tokio::test]
async fn origin_must_match_exactly() {
    let cases = [
        ("https://app.example.com", true),
        ("HTTPS://APP.EXAMPLE.COM", true),
        ("https://app.example.com:443", true),
        ("http://localhost:3000", true),
        ("http://app.example.com", false),
        ("https://app.example.com:8443", false),
        ("https://evil.app.example.com", false),
        ("https://app.example.com.evil.test", false),
        ("https://example.com", false),
        ("http://localhost", false),
        ("null", false),
        ("", false),
        ("*", false),
        ("https://app.example.com/", false),
        ("https://app.example.com/path", false),
    ];
    for (origin, ok) in cases {
        let got = status(Method::POST, &[("origin", origin)]).await;
        assert_eq!(got != StatusCode::FORBIDDEN, ok, "{origin:?}");
    }
}

#[tokio::test]
async fn referer_is_used_only_without_origin() {
    let ok = [("referer", "https://app.example.com/form?a=1#top")];
    assert_eq!(status(Method::POST, &ok).await, StatusCode::OK);
    let port = [("referer", "https://app.example.com:443/x")];
    assert_eq!(status(Method::POST, &port).await, StatusCode::OK);
    let foreign = [("referer", "https://evil.test/app.example.com")];
    assert_eq!(status(Method::POST, &foreign).await, 403);
    let tricky = [("referer", "https://app.example.com.evil.test/")];
    assert_eq!(status(Method::POST, &tricky).await, 403);
    let userinfo = [("referer", "https://app.example.com@evil.test/")];
    assert_eq!(status(Method::POST, &userinfo).await, 403);
    let garbage = [("referer", "not a url")];
    assert_eq!(status(Method::POST, &garbage).await, 403);
    // A present Origin decides; a good Referer cannot rescue it.
    let both = [
        ("origin", "https://evil.test"),
        ("referer", "https://app.example.com/"),
    ];
    assert_eq!(status(Method::POST, &both).await, 403);
    let null = [("origin", "null"), ("referer", "https://app.example.com/")];
    assert_eq!(status(Method::POST, &null).await, 403);
}

#[tokio::test]
async fn repeated_headers_are_rejected() {
    let two_origins = [("origin", APP), ("origin", APP)];
    assert_eq!(status(Method::POST, &two_origins).await, 403);
    let two_referers = [
        ("referer", "https://app.example.com/"),
        ("referer", "https://app.example.com/"),
    ];
    assert_eq!(status(Method::POST, &two_referers).await, 403);
    let two_sites = [
        ("origin", APP),
        ("sec-fetch-site", "same-origin"),
        ("sec-fetch-site", "same-origin"),
    ];
    assert_eq!(status(Method::POST, &two_sites).await, 403);
}

#[tokio::test]
async fn sec_fetch_site_values() {
    let same = [("sec-fetch-site", "same-origin")];
    assert_eq!(status(Method::POST, &same).await, StatusCode::OK);
    let upper = [("sec-fetch-site", "Same-Origin")];
    assert_eq!(status(Method::POST, &upper).await, StatusCode::OK);
    // Same-origin passes even when Origin is a foreign value the browser
    // would never send.
    let cross = [("sec-fetch-site", "cross-site"), ("origin", APP)];
    assert_eq!(status(Method::POST, &cross).await, 403);
    let cross = [("sec-fetch-site", "Cross-Site")];
    assert_eq!(status(Method::POST, &cross).await, 403);
    for value in ["same-site", "none", "bogus"] {
        let alone = [("sec-fetch-site", value)];
        assert_eq!(status(Method::POST, &alone).await, 403, "{value}");
        let with = [("sec-fetch-site", value), ("origin", APP)];
        assert_eq!(status(Method::POST, &with).await, 200, "{value}");
        let evil = [("sec-fetch-site", value), ("origin", "https://e.test")];
        assert_eq!(status(Method::POST, &evil).await, 403, "{value}");
    }
}

#[tokio::test]
async fn host_and_forwarded_headers_never_widen_the_allow_list() {
    let spoofed = [
        ("host", "evil.test"),
        ("origin", "https://evil.test"),
        ("x-forwarded-host", "evil.test"),
        ("x-forwarded-proto", "https"),
        ("forwarded", "host=evil.test;proto=https"),
    ];
    assert_eq!(status(Method::POST, &spoofed).await, 403);
    let same_host = [("host", "app.example.com"), ("origin", "https://x.t")];
    assert_eq!(status(Method::POST, &same_host).await, 403);
}

#[tokio::test]
async fn rejection_is_one_generic_problem() {
    let attempts: [&[(&str, &str)]; 5] = [
        &[],
        &[("origin", "null")],
        &[("origin", "https://evil.test")],
        &[("origin", APP), ("sec-fetch-site", "cross-site")],
        &[("origin", APP), ("origin", APP)],
    ];
    let mut bodies = Vec::new();
    for headers in attempts {
        let (status, body) = send(csrf(), Method::POST, "/x", headers).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], 403);
        assert_eq!(json["title"], "Forbidden");
        bodies.push(body);
    }
    assert!(bodies.windows(2).all(|pair| pair[0] == pair[1]));
}

#[tokio::test]
async fn exemptions_match_whole_segments() {
    let exempt = || csrf().exempt("/hooks").exempt("relative");
    let paths = [
        ("/hooks/a", StatusCode::OK),
        ("/hooksy", StatusCode::FORBIDDEN),
        ("/hooks/../x", StatusCode::FORBIDDEN),
        ("/x", StatusCode::FORBIDDEN),
    ];
    for (path, want) in paths {
        let (status, _) = send(exempt(), Method::POST, path, &[]).await;
        assert_eq!(status, want, "{path}");
    }
    let slash = csrf().exempt("/hooks/");
    let (status, _) = send(slash.clone(), Method::POST, "/hooks/a", &[]).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(slash, Method::POST, "/hooks", &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let relative = csrf().exempt("relative");
    let (status, _) = send(relative, Method::POST, "/x", &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

async fn lax_status(headers: &[(&str, &str)]) -> StatusCode {
    let csrf = csrf().only_with_cookie();
    send(csrf, Method::POST, "/x", headers).await.0
}

#[tokio::test]
async fn only_with_cookie_skips_credential_free_requests() {
    assert_eq!(lax_status(&[]).await, StatusCode::OK);
    assert_eq!(lax_status(&[("authorization", "Bearer t")]).await, 200);
    assert_eq!(lax_status(&[("cookie", "a=b")]).await, 403);
    assert_eq!(lax_status(&[("authorization", "Basic dTpw")]).await, 403);
    assert_eq!(lax_status(&[("authorization", "basic dTpw")]).await, 403);
    let good = [("cookie", "a=b"), ("origin", APP)];
    assert_eq!(lax_status(&good).await, StatusCode::OK);
    let evil = [("cookie", "a=b"), ("origin", "https://evil.test")];
    assert_eq!(lax_status(&evil).await, 403);
    // Default: credential-free requests are checked too.
    assert_eq!(status(Method::POST, &[]).await, 403);
}
