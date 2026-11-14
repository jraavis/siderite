//! Request ids and structured request logging.

use super::adapt::{BoxService, Next, from_fn, impl_layer};
use crate::error::ApiError;
use crate::extract::{FromRequestParts, Request};
use crate::response::Response;
use http::request::Parts;
use http::{HeaderName, HeaderValue};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;
use tower::Layer;
use tracing::Instrument;

const DEFAULT_HEADER: &str = "x-request-id";
const MAX_ID_LEN: usize = 128;

/// The id of the current request, set by the [`RequestIdLayer`] middleware.
///
/// Usable as a handler argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromRequestParts for RequestId {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<RequestId>()
            .cloned()
            .ok_or_else(|| ApiError::internal("RequestId used without the request_id middleware"))
    }
}

/// Assigns every request an id: an incoming `x-request-id` is propagated when
/// it is 1..=128 visible ASCII characters, otherwise a UUID v4 is generated.
/// The id is exposed as [`RequestId`], forwarded to the handler as a request
/// header and echoed on the response.
#[derive(Debug, Clone)]
pub struct RequestIdLayer {
    header: HeaderName,
}

impl Default for RequestIdLayer {
    fn default() -> Self {
        Self {
            header: HeaderName::from_static(DEFAULT_HEADER),
        }
    }
}

impl RequestIdLayer {
    /// Use the default `x-request-id` header.
    pub fn new() -> Self {
        Self::default()
    }

    /// Use another header name.
    #[must_use]
    pub fn header(mut self, name: HeaderName) -> Self {
        self.header = name;
        self
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let header = self.header.clone();
        from_fn(move |mut req: Request, next: Next| {
            let header = header.clone();
            async move {
                let id = req
                    .headers()
                    .get(&header)
                    .and_then(|v| v.to_str().ok())
                    .filter(|v| valid_id(v))
                    .map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned);
                let value = HeaderValue::from_str(&id).ok();
                if let Some(value) = &value {
                    req.headers_mut().insert(header.clone(), value.clone());
                }
                req.extensions_mut().insert(RequestId(id));
                let mut response = next.run(req).await;
                if let Some(value) = value {
                    response.headers_mut().insert(header, value);
                }
                response
            }
        })
        .layer(inner)
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_LEN && id.bytes().all(|b| b.is_ascii_graphic())
}

/// Filled with the matched route template by the DI layer (which runs after
/// routing) so outer middleware can read it once the response is ready.
#[derive(Clone, Default)]
pub(crate) struct RouteSlot(Arc<Mutex<Option<String>>>);

impl RouteSlot {
    fn set(&self, route: String) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(route);
    }

    fn get(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Record the matched route in the request's [`RouteSlot`], if any.
pub(crate) fn note_matched_path(extensions: &http::Extensions) {
    if let (Some(slot), Some(path)) = (
        extensions.get::<RouteSlot>(),
        extensions.get::<axum::extract::MatchedPath>(),
    ) {
        slot.set(path.as_str().to_owned());
    }
}

/// Emits an `http.request` tracing span per request with `request_id`,
/// `method`, `route` (matched path template via axum `MatchedPath`, or
/// `"<unmatched>"`), `status` and `latency_ms`.
///
/// **Privacy:** only those fields are recorded. Headers (Authorization,
/// Cookie, API keys, ...), the query string and bodies are never logged.
/// Register [`RequestIdLayer`] first so the id is available.
#[derive(Debug, Clone, Default)]
pub struct RequestLogging;

impl RequestLogging {
    /// Create the middleware.
    pub fn new() -> Self {
        Self
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        from_fn(|mut req: Request, next: Next| async move {
            let start = Instant::now();
            let slot = RouteSlot::default();
            req.extensions_mut().insert(slot.clone());
            let request_id = req
                .extensions()
                .get::<RequestId>()
                .map_or_else(|| "-".to_owned(), |id| id.0.clone());
            let method = req.method().clone();
            let span = tracing::info_span!(
                "http.request",
                request_id = %request_id,
                method = %method,
                route = tracing::field::Empty,
                status = tracing::field::Empty,
                latency_ms = tracing::field::Empty,
            );
            let response: Response = next.run(req).instrument(span.clone()).await;
            let status = response.status().as_u16();
            let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
            let route = slot.get().unwrap_or_else(|| "<unmatched>".to_owned());
            span.record("route", tracing::field::display(&route));
            span.record("status", status);
            span.record("latency_ms", latency_ms);
            tracing::info!(parent: &span, %route, status, latency_ms, "request completed");
            response
        })
        .layer(inner)
    }
}

impl_layer!(RequestIdLayer, RequestLogging);

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::app::App;
    use crate::body::Body;
    use crate::routing::get;
    use http::Request;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

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

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            use std::fmt::Write as _;
            let _ = write!(self.0, "{}={value} ", field.name());
        }

        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            use std::fmt::Write as _;
            let _ = write!(self.0, "{}={value} ", field.name());
        }

        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            use std::fmt::Write as _;
            let _ = write!(self.0, "{}={value} ", field.name());
        }

        fn record_u128(&mut self, field: &tracing::field::Field, value: u128) {
            use std::fmt::Write as _;
            let _ = write!(self.0, "{}={value} ", field.name());
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

    async fn send(app: App, path: &str, headers: &[(&str, &str)]) -> crate::http::StatusCode {
        let mut req = Request::builder().method("GET").uri(path);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let service = app.into_router_service().unwrap();
        service
            .oneshot(req.body(Body::from("")).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn http_request_span_records_fields_and_never_secrets() {
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.clone());
        let app = App::new()
            .route("/users/{id}", get(|| async { "u" }))
            .request_id()
            .request_logging();
        let status = send(
            app,
            "/users/42?api_key=QUERYSECRET",
            &[
                ("authorization", "Bearer TOPSECRET"),
                ("cookie", "session=COOKIESECRET"),
                ("x-api-key", "KEYSECRET"),
                ("x-request-id", "req-1"),
            ],
        )
        .await;
        assert_eq!(status, crate::http::StatusCode::OK);
        let logged = capture.lines.lock().unwrap().join("\n");
        assert!(logged.contains("span http.request"), "span name:\n{logged}");
        for secret in [
            "TOPSECRET",
            "COOKIESECRET",
            "KEYSECRET",
            "QUERYSECRET",
            "api_key=",
        ] {
            assert!(
                !logged.contains(secret),
                "{secret} leaked into logs:\n{logged}"
            );
        }
        assert!(
            logged.contains("request_id=req-1") || logged.contains("request_id=\"req-1\""),
            "{logged}"
        );
        assert!(logged.contains("/users/{id}"), "matched route:\n{logged}");
        assert!(logged.contains("status=200"), "{logged}");
        assert!(logged.contains("latency_ms="), "{logged}");
        assert!(
            logged.contains("method=GET") || logged.contains("method=\"GET\""),
            "{logged}"
        );
        assert!(!logged.contains("/users/42?"), "{logged}");
        assert!(
            !logged.contains("Authorization") && !logged.contains("Bearer "),
            "{logged}"
        );
    }

    #[tokio::test]
    async fn unmatched_route_is_recorded() {
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(capture.clone());
        let app = App::new()
            .route("/hello", get(|| async { "h" }))
            .request_id()
            .request_logging();
        let status = send(
            app,
            "/missing?token=QUERYSECRET",
            &[("authorization", "Bearer TOPSECRET")],
        )
        .await;
        assert_eq!(status, crate::http::StatusCode::NOT_FOUND);
        let logged = capture.lines.lock().unwrap().join("\n");
        assert!(logged.contains("<unmatched>"), "{logged}");
        assert!(logged.contains("status=404"), "{logged}");
        assert!(!logged.contains("QUERYSECRET"), "{logged}");
        assert!(!logged.contains("TOPSECRET"), "{logged}");
    }
}
