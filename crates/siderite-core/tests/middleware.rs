//! Middleware: ordering, custom layers and every built-in.
#![allow(clippy::unwrap_used)]

use ::http::Method;
use serde_json::Value;
use siderite_core::http::StatusCode;
use siderite_core::middleware::{
    BodyLimit, Compression, ConcurrencyLimit, Cors, CsrfOrigin, HttpsRedirect, RateLimit,
    RequestId, RequestIdLayer, Timeout, TrustedHosts, from_fn,
};
use siderite_core::*;
use siderite_testkit::{TestClient, TestResponse};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

async fn call(
    client: &TestClient,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
    body: &'static str,
) -> TestResponse {
    let mut req = ::http::Request::builder().method(method).uri(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    client
        .send(req.body(Body::from(body)).unwrap())
        .await
        .unwrap()
}

async fn get_with(client: &TestClient, path: &str, headers: &[(&str, &str)]) -> TestResponse {
    call(client, Method::GET, path, headers, "").await
}

fn header<'a>(res: &'a TestResponse, name: &str) -> Option<&'a str> {
    res.headers.get(name)?.to_str().ok()
}

fn assert_problem(res: &TestResponse, status: StatusCode) {
    assert_eq!(res.status, status);
    assert_eq!(res.content_type(), Some("application/problem+json"));
    let body: Value = res.json().unwrap();
    assert_eq!(body["status"], status.as_u16());
}

fn hello_app() -> App {
    App::new().route("/hello", get(|| async { "hello" }))
}

// ---- ordering & custom layers -----------------------------------------------

#[tokio::test]
async fn first_registered_layer_is_outermost() {
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let recorder = |name: &'static str| {
        let log = Arc::clone(&log);
        from_fn(move |req: Request, next: middleware::Next| {
            let log = Arc::clone(&log);
            async move {
                log.lock().unwrap().push(format!("{name}-in"));
                let response = next.run(req).await;
                log.lock().unwrap().push(format!("{name}-out"));
                response
            }
        })
    };
    let handler_log = Arc::clone(&log);
    let app = App::new()
        .layer(recorder("A"))
        .with_state(1_u8)
        .layer(recorder("B"))
        .route(
            "/x",
            get(move || {
                let log = Arc::clone(&handler_log);
                async move {
                    log.lock().unwrap().push("handler".to_owned());
                    "x"
                }
            }),
        )
        .layer(recorder("C"));
    assert_eq!(TestClient::new(app).get("/x").await.unwrap().text(), "x");
    assert_eq!(
        *log.lock().unwrap(),
        ["A-in", "B-in", "C-in", "handler", "C-out", "B-out", "A-out"]
    );
}

#[tokio::test]
async fn middleware_wraps_not_found_and_docs_but_child_layers_stay_scoped() {
    let stamp = |value: &'static str| {
        from_fn(move |req: Request, next: middleware::Next| async move {
            let mut response = next.run(req).await;
            response
                .headers_mut()
                .insert("x-stamp", ::http::HeaderValue::from_static(value));
            response
        })
    };
    let child = App::new()
        .route("/c", get(|| async { "c" }))
        .layer(stamp("child"));
    let app = hello_app().mount("/m", child).layer(stamp("root"));
    let client = TestClient::new(app);
    assert_eq!(
        header(&client.get("/missing").await.unwrap(), "x-stamp"),
        Some("root")
    );
    assert_eq!(
        header(&client.get("/openapi.json").await.unwrap(), "x-stamp"),
        Some("root")
    );
    assert_eq!(
        header(&client.get("/hello").await.unwrap(), "x-stamp"),
        Some("root")
    );
    let child_res = client.get("/m/c").await.unwrap();
    assert_eq!(child_res.text(), "c");
    // The root layer is outermost, so it has the last word on the header.
    assert_eq!(header(&child_res, "x-stamp"), Some("root"));
}

#[tokio::test]
async fn child_layer_applies_only_to_child_routes() {
    let mark = from_fn(|req: Request, next: middleware::Next| async move {
        let mut response = next.run(req).await;
        response
            .headers_mut()
            .insert("x-child", ::http::HeaderValue::from_static("yes"));
        response
    });
    let child = App::new().route("/c", get(|| async { "c" })).layer(mark);
    let client = TestClient::new(hello_app().mount("/m", child));
    assert_eq!(
        header(&client.get("/m/c").await.unwrap(), "x-child"),
        Some("yes")
    );
    assert_eq!(
        header(&client.get("/hello").await.unwrap(), "x-child"),
        None
    );
}

// ---- CORS ---------------------------------------------------------------------

#[tokio::test]
async fn cors_preflight_and_simple_requests() {
    let app = hello_app().cors(
        Cors::new()
            .allow_origin("https://app.example.com")
            .allow_method(Method::GET)
            .allow_header("x-custom")
            .max_age(Duration::from_secs(600)),
    );
    let client = TestClient::new(app);
    let preflight = call(
        &client,
        Method::OPTIONS,
        "/hello",
        &[
            ("origin", "https://app.example.com"),
            ("access-control-request-method", "GET"),
            ("access-control-request-headers", "x-custom"),
        ],
        "",
    )
    .await;
    assert!(preflight.status.is_success());
    assert_eq!(
        header(&preflight, "access-control-allow-origin"),
        Some("https://app.example.com")
    );
    assert_eq!(header(&preflight, "access-control-max-age"), Some("600"));
    let ok = get_with(&client, "/hello", &[("origin", "https://app.example.com")]).await;
    assert_eq!(
        header(&ok, "access-control-allow-origin"),
        Some("https://app.example.com")
    );
    let denied = get_with(&client, "/hello", &[("origin", "https://evil.example")]).await;
    assert_eq!(denied.text(), "hello");
    assert_eq!(header(&denied, "access-control-allow-origin"), None);
}

#[tokio::test]
async fn cors_permissive_and_credentials_never_use_wildcard() {
    let client = TestClient::new(hello_app().cors(Cors::permissive()));
    let res = get_with(&client, "/hello", &[("origin", "https://a.example")]).await;
    assert_eq!(header(&res, "access-control-allow-origin"), Some("*"));

    let creds = hello_app().cors(Cors::permissive().allow_credentials(true));
    let client = TestClient::new(creds);
    let res = get_with(&client, "/hello", &[("origin", "https://a.example")]).await;
    assert_eq!(
        header(&res, "access-control-allow-origin"),
        Some("https://a.example")
    );
    assert_eq!(
        header(&res, "access-control-allow-credentials"),
        Some("true")
    );
}

// ---- compression --------------------------------------------------------------

#[tokio::test]
async fn compression_negotiates_encoding() {
    let big = "abcdefghij".repeat(500);
    let app = App::new()
        .route(
            "/big",
            get(move || {
                let big = big.clone();
                async move { big }
            }),
        )
        .compression(Compression::new());
    let client = TestClient::new(app);
    let gz = get_with(&client, "/big", &[("accept-encoding", "gzip")]).await;
    assert_eq!(header(&gz, "content-encoding"), Some("gzip"));
    assert!(gz.body.len() < 5000);
    let br = get_with(&client, "/big", &[("accept-encoding", "br")]).await;
    assert_eq!(header(&br, "content-encoding"), Some("br"));
    let plain = get_with(&client, "/big", &[]).await;
    assert_eq!(header(&plain, "content-encoding"), None);
    assert_eq!(plain.body.len(), 5000);
}

#[tokio::test]
async fn compression_can_disable_brotli() {
    let big = "abcdefghij".repeat(500);
    let app = App::new()
        .route(
            "/big",
            get(move || {
                let big = big.clone();
                async move { big }
            }),
        )
        .compression(Compression::new().brotli(false));
    let res = get_with(
        &TestClient::new(app),
        "/big",
        &[("accept-encoding", "br, gzip")],
    )
    .await;
    assert_eq!(header(&res, "content-encoding"), Some("gzip"));
}

// ---- trusted hosts -------------------------------------------------------------

#[tokio::test]
async fn trusted_hosts_allow_list() {
    let app = hello_app().trusted_hosts(TrustedHosts::new(["example.com", "*.example.org"]));
    let client = TestClient::new(app);
    for host in [
        "example.com",
        "EXAMPLE.com:8080",
        "api.example.org",
        "a.b.example.org",
    ] {
        assert_eq!(
            get_with(&client, "/hello", &[("host", host)]).await.status,
            StatusCode::OK,
            "{host}"
        );
    }
    for host in [
        "evil.com",
        "example.org",
        "notexample.com",
        "example.com.evil.com",
    ] {
        assert_problem(
            &get_with(&client, "/hello", &[("host", host)]).await,
            StatusCode::BAD_REQUEST,
        );
    }
    assert_problem(
        &client.get("/hello").await.unwrap(),
        StatusCode::BAD_REQUEST,
    );
}

// ---- https redirect ------------------------------------------------------------

#[tokio::test]
async fn https_redirect() {
    let client = TestClient::new(hello_app().https_redirect(HttpsRedirect::new()));
    let res = get_with(&client, "/hello?x=1", &[("host", "example.com")]).await;
    assert_eq!(res.status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(
        header(&res, "location"),
        Some("https://example.com/hello?x=1")
    );
    let secure = get_with(
        &client,
        "/hello",
        &[("host", "example.com"), ("x-forwarded-proto", "https")],
    )
    .await;
    assert_eq!(secure.status, StatusCode::PERMANENT_REDIRECT);
    assert_problem(
        &client.get("/hello").await.unwrap(),
        StatusCode::BAD_REQUEST,
    );

    let strict = TestClient::new(
        hello_app().https_redirect(HttpsRedirect::new().trust_forwarded_proto(false)),
    );
    let res = get_with(
        &strict,
        "/hello",
        &[("host", "example.com"), ("x-forwarded-proto", "https")],
    )
    .await;
    assert_eq!(res.status, StatusCode::PERMANENT_REDIRECT);
}

// ---- request id ----------------------------------------------------------------

async fn echo_id(id: RequestId) -> String {
    id.to_string()
}

#[tokio::test]
async fn request_id_generated_propagated_and_extractable() {
    let app = App::new().route("/id", get(echo_id)).request_id();
    let client = TestClient::new(app);
    let generated = client.get("/id").await.unwrap();
    let id = header(&generated, "x-request-id").unwrap().to_owned();
    assert_eq!(id.len(), 36, "uuid v4");
    assert_eq!(generated.text(), id);
    let propagated = get_with(&client, "/id", &[("x-request-id", "abc-123")]).await;
    assert_eq!(header(&propagated, "x-request-id"), Some("abc-123"));
    assert_eq!(propagated.text(), "abc-123");
    let replaced = get_with(&client, "/id", &[("x-request-id", "has space")]).await;
    assert_ne!(header(&replaced, "x-request-id"), Some("has space"));
    let custom = App::new()
        .route("/id", get(echo_id))
        .layer(RequestIdLayer::new().header(::http::HeaderName::from_static("x-trace")));
    let res = get_with(&TestClient::new(custom), "/id", &[("x-trace", "t1")]).await;
    assert_eq!(header(&res, "x-trace"), Some("t1"));
}

#[tokio::test]
async fn request_id_extractor_without_middleware_is_a_500() {
    let client = TestClient::new(App::new().route("/id", get(echo_id)));
    assert_problem(
        &client.get("/id").await.unwrap(),
        StatusCode::INTERNAL_SERVER_ERROR,
    );
}

// ---- request logging -----------------------------------------------------------

#[derive(Clone, Default)]
struct Capture {
    lines: Arc<Mutex<Vec<String>>>,
    next_id: Arc<AtomicUsize>,
}

struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut fields = Fields(format!("span {} ", attrs.metadata().name()));
        attrs.record(&mut fields);
        self.lines.lock().unwrap().push(fields.0);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) as u64 + 1;
        tracing::span::Id::from_u64(id)
    }

    fn record(&self, _: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        let mut fields = Fields("record ".to_owned());
        values.record(&mut fields);
        self.lines.lock().unwrap().push(fields.0);
    }

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields("event ".to_owned());
        event.record(&mut fields);
        self.lines.lock().unwrap().push(fields.0);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

#[tokio::test]
async fn request_logging_records_metadata_but_never_secrets() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let app = App::new()
        .route("/users/{id}", get(|| async { "u" }))
        .request_id()
        .request_logging();
    let client = TestClient::new(app);
    let res = get_with(
        &client,
        "/users/42?api_key=QUERYSECRET",
        &[
            ("authorization", "Bearer TOPSECRET"),
            ("cookie", "session=COOKIESECRET"),
            ("x-api-key", "KEYSECRET"),
            ("x-request-id", "req-1"),
        ],
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    let logged = capture.lines.lock().unwrap().join("\n");
    for secret in ["TOPSECRET", "COOKIESECRET", "KEYSECRET", "QUERYSECRET"] {
        assert!(
            !logged.contains(secret),
            "{secret} leaked into logs:\n{logged}"
        );
    }
    assert!(
        logged.contains("request_id=req-1") || logged.contains("request_id=\"req-1\""),
        "{logged}"
    );
    assert!(
        logged.contains("/users/{id}"),
        "matched route is logged:\n{logged}"
    );
    assert!(logged.contains("status=200"), "{logged}");
    assert!(logged.contains("latency_ms="), "{logged}");
    assert!(logged.contains("method=GET"), "{logged}");
}

// ---- timeout -------------------------------------------------------------------

fn slow_app(sleep_ms: u64) -> App {
    App::new().route(
        "/slow",
        get(move || async move {
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
            "done"
        }),
    )
}

#[tokio::test]
async fn timeout_returns_504_problem_by_default() {
    let client = TestClient::new(slow_app(500).timeout(Duration::from_millis(30)));
    assert_problem(
        &client.get("/slow").await.unwrap(),
        StatusCode::GATEWAY_TIMEOUT,
    );
    let fast = TestClient::new(slow_app(1).timeout(Duration::from_secs(5)));
    assert_eq!(fast.get("/slow").await.unwrap().text(), "done");
}

#[tokio::test]
async fn timeout_status_is_configurable() {
    let app = slow_app(500)
        .layer(Timeout::new(Duration::from_millis(30)).status(StatusCode::REQUEST_TIMEOUT));
    assert_problem(
        &TestClient::new(app).get("/slow").await.unwrap(),
        StatusCode::REQUEST_TIMEOUT,
    );
}

// ---- concurrency ---------------------------------------------------------------

#[tokio::test]
async fn concurrency_limit_serialises_requests() {
    let (current, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (c, p) = (Arc::clone(&current), Arc::clone(&peak));
    let app = App::new()
        .route(
            "/w",
            get(move || {
                let (c, p) = (Arc::clone(&c), Arc::clone(&p));
                async move {
                    let now = c.fetch_add(1, Ordering::SeqCst) + 1;
                    p.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    c.fetch_sub(1, Ordering::SeqCst);
                    "w"
                }
            }),
        )
        .layer(ConcurrencyLimit::new(1).queue(3, Duration::from_secs(1)));
    let client = TestClient::new(app);
    let all = futures_util::future::join_all((0..4).map(|_| client.get("/w"))).await;
    assert!(
        all.iter()
            .all(|r| r.as_ref().unwrap().status == StatusCode::OK)
    );
    assert_eq!(peak.load(Ordering::SeqCst), 1);
}

// ---- body limit ----------------------------------------------------------------

#[tokio::test]
async fn body_limit_rejects_oversized_bodies() {
    let app = App::new()
        .route("/echo", post(|Json(v): Json<Value>| async move { Json(v) }))
        .layer(BodyLimit::new(16));
    let client = TestClient::new(app);
    let small = call(
        &client,
        Method::POST,
        "/echo",
        &[
            ("content-type", "application/json"),
            ("content-length", "2"),
        ],
        "{}",
    )
    .await;
    assert_eq!(small.status, StatusCode::OK);
    let big = r#"{"key":"aaaaaaaaaaaaaaaaaaaaaaaa"}"#;
    let declared = call(
        &client,
        Method::POST,
        "/echo",
        &[
            ("content-type", "application/json"),
            ("content-length", "34"),
        ],
        big,
    )
    .await;
    assert_problem(&declared, StatusCode::PAYLOAD_TOO_LARGE);
    let streamed = call(
        &client,
        Method::POST,
        "/echo",
        &[("content-type", "application/json")],
        big,
    )
    .await;
    assert_eq!(streamed.status, StatusCode::PAYLOAD_TOO_LARGE);
}

// ---- rate limit ----------------------------------------------------------------

#[tokio::test]
async fn rate_limit_token_bucket_with_retry_after() {
    let client = TestClient::new(hello_app().rate_limit(RateLimit::new(2, 0.5)));
    assert_eq!(client.get("/hello").await.unwrap().status, StatusCode::OK);
    assert_eq!(client.get("/hello").await.unwrap().status, StatusCode::OK);
    let limited = client.get("/hello").await.unwrap();
    assert_problem(&limited, StatusCode::TOO_MANY_REQUESTS);
    let retry: u64 = header(&limited, "retry-after").unwrap().parse().unwrap();
    assert!((1..=2).contains(&retry), "retry-after {retry}");
}

#[tokio::test]
async fn rate_limit_refills_over_time() {
    let client = TestClient::new(hello_app().rate_limit(RateLimit::new(1, 100.0)));
    assert_eq!(client.get("/hello").await.unwrap().status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(client.get("/hello").await.unwrap().status, StatusCode::OK);
}

#[tokio::test]
async fn rate_limit_keys_by_forwarded_for_only_when_trusted() {
    let trusted =
        TestClient::new(hello_app().rate_limit(RateLimit::new(1, 0.1).trust_forwarded_for(true)));
    let a = [("x-forwarded-for", "1.1.1.1, 10.0.0.1")];
    let b = [("x-forwarded-for", "2.2.2.2")];
    assert_eq!(
        get_with(&trusted, "/hello", &a).await.status,
        StatusCode::OK
    );
    assert_eq!(
        get_with(&trusted, "/hello", &b).await.status,
        StatusCode::OK
    );
    assert_eq!(
        get_with(&trusted, "/hello", &a).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );

    let untrusted = TestClient::new(hello_app().rate_limit(RateLimit::new(1, 0.1)));
    assert_eq!(
        get_with(&untrusted, "/hello", &a).await.status,
        StatusCode::OK
    );
    assert_eq!(
        get_with(&untrusted, "/hello", &b).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn request_logging_records_nested_docs_and_unmatched_routes() {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());
    let child = App::new().route("/items/{id}", get(|| async { "i" }));
    let app = App::new().mount("/api", child).request_logging();
    let client = TestClient::new(app);
    for (path, status, route) in [
        ("/api/items/7", StatusCode::OK, "route=/api/items/{id}"),
        ("/openapi.json", StatusCode::OK, "route=/openapi.json"),
        ("/nope", StatusCode::NOT_FOUND, "route=<unmatched>"),
    ] {
        capture.lines.lock().unwrap().clear();
        let res = get_with(&client, path, &[]).await;
        assert_eq!(res.status, status, "{path}");
        let logged = capture.lines.lock().unwrap().join("\n");
        assert!(
            logged.contains(route),
            "{path}: expected {route} in\n{logged}"
        );
    }
}

// ---- CSRF origin ------------------------------------------------------

#[tokio::test]
async fn csrf_origin_blocks_cross_site_writes_only() {
    let csrf = CsrfOrigin::new(["https://app.example.com"]).unwrap();
    let app = App::new()
        .route("/w", get(|| async { "r" }).post(|| async { "w" }))
        .csrf_origin(csrf);
    let client = TestClient::new(app);
    let write = |headers: &'static [(&'static str, &'static str)]| {
        call(&client, Method::POST, "/w", headers, "")
    };
    let evil = write(&[("origin", "https://evil.test")]).await;
    assert_problem(&evil, StatusCode::FORBIDDEN);
    assert_problem(&write(&[]).await, StatusCode::FORBIDDEN);
    let own = write(&[("origin", "https://app.example.com")]).await;
    assert_eq!(own.status, StatusCode::OK);
    let same = write(&[("sec-fetch-site", "same-origin")]).await;
    assert_eq!(same.status, StatusCode::OK);
    let foreign = [("origin", "https://evil.test")];
    let read = get_with(&client, "/w", &foreign).await;
    assert_eq!(read.status, StatusCode::OK);
}
