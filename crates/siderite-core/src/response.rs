//! Responses.
//!
//! siderite owns [`IntoResponse`] so response types document themselves for
//! OpenAPI through [`IntoResponse::describe`] (default: nothing). Handlers may
//! return `impl IntoResponse`.

use crate::body::Body;
use crate::error::ApiError;
use http::{HeaderValue, StatusCode, header};
use siderite_openapi::{Operation, Schema, SchemaRegistry};
use siderite_validation::{Dump, DumpOptions};

/// An HTTP response with a siderite [`Body`].
pub type Response = http::Response<Body>;

/// Status code under which handler return types document success.
pub const SUCCESS: &str = "200";

/// Convert a value into an HTTP response.
pub trait IntoResponse {
    /// Build the response.
    fn into_response(self) -> Response;

    /// Document the responses this type can produce.
    fn describe(_op: &mut Operation, _registry: &mut SchemaRegistry) {}
}

/// JSON body (response) or JSON request extractor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Json<T>(pub T);

/// HTML response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Html<T>(pub T);

/// `text/plain` response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainText<T>(pub T);

/// `204 No Content` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoContent;

/// Overrides the status code of an inner response.
///
/// The status is only known at runtime, so the OpenAPI document still lists
/// the inner response under its own code (usually `200`). To document a
/// fixed non-200 success status, set it on the route with
/// [`MethodRouter::status`](crate::routing::MethodRouter::status) instead
/// and return the plain body from the handler.
#[derive(Debug, Clone, Copy)]
pub struct WithStatus<R>(pub StatusCode, pub R);

/// Build a response with `body` and a `Content-Type`.
pub(crate) fn with_content_type(content_type: &'static str, body: impl Into<Body>) -> Response {
    let mut response = Response::new(body.into());
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

/// Build a bodiless response with `status`.
pub(crate) fn status_only(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

impl IntoResponse for Response {
    fn into_response(self) -> Response {
        self
    }
}

/// Serialize `value` with `opts` into a JSON response.
fn dump_response<T: Dump + ?Sized>(value: &T, opts: &DumpOptions) -> Response {
    let value = siderite_validation::dump::DumpSerialize(value, opts);
    match serde_json::to_vec(&value) {
        Ok(bytes) => with_content_type("application/json", bytes),
        Err(err) => ApiError::internal(err).into_response(),
    }
}

/// JSON response serialized with explicit [`DumpOptions`]
/// (FastAPI `response_model_exclude_none` and friends).
#[derive(Debug, Clone)]
pub struct JsonDump<T>(pub T, pub DumpOptions);

impl<T: Dump + Schema + 'static> IntoResponse for JsonDump<T> {
    fn into_response(self) -> Response {
        dump_response(&self.0, &self.1)
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        <Json<T> as IntoResponse>::describe(op, registry);
    }
}

impl<T: Dump + Schema + 'static> IntoResponse for Json<T> {
    /// Serialized through [`Dump`], so computed fields and serializers apply.
    fn into_response(self) -> Response {
        dump_response(&self.0, &DumpOptions::default())
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        let schema = registry.subschema::<T>();
        op.add_response(
            SUCCESS,
            "Successful Response",
            Some(("application/json", schema)),
        );
    }
}

macro_rules! text_like {
    ($($ty:ident => $mime:literal),*) => {$(
        impl<T: Into<String>> IntoResponse for $ty<T> {
            fn into_response(self) -> Response {
                with_content_type($mime, self.0.into())
            }

            fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
                let schema = registry.subschema::<String>();
                op.add_response(SUCCESS, "Successful Response", Some(($mime, schema)));
            }
        }
    )*};
}
text_like!(Html => "text/html; charset=utf-8", PlainText => "text/plain; charset=utf-8");

impl IntoResponse for &'static str {
    fn into_response(self) -> Response {
        PlainText(self).into_response()
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        <PlainText<String> as IntoResponse>::describe(op, registry);
    }
}

impl IntoResponse for String {
    fn into_response(self) -> Response {
        PlainText(self).into_response()
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        <PlainText<String> as IntoResponse>::describe(op, registry);
    }
}

impl IntoResponse for () {
    fn into_response(self) -> Response {
        status_only(StatusCode::OK)
    }

    fn describe(op: &mut Operation, _: &mut SchemaRegistry) {
        op.add_response(SUCCESS, "Successful Response", None);
    }
}

impl IntoResponse for StatusCode {
    fn into_response(self) -> Response {
        status_only(self)
    }
}

impl IntoResponse for NoContent {
    fn into_response(self) -> Response {
        status_only(StatusCode::NO_CONTENT)
    }

    fn describe(op: &mut Operation, _: &mut SchemaRegistry) {
        op.add_response("204", "No Content", None);
    }
}

impl<R: IntoResponse> IntoResponse for WithStatus<R> {
    fn into_response(self) -> Response {
        let mut response = self.1.into_response();
        *response.status_mut() = self.0;
        response
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        R::describe(op, registry);
    }
}

/// Same as [`WithStatus`]: the runtime status is not documented. Use
/// [`MethodRouter::status`](crate::routing::MethodRouter::status) for a
/// documented non-200 success status.
impl<R: IntoResponse> IntoResponse for (StatusCode, R) {
    fn into_response(self) -> Response {
        WithStatus(self.0, self.1).into_response()
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        R::describe(op, registry);
    }
}

impl<T: IntoResponse, E: IntoResponse> IntoResponse for Result<T, E> {
    fn into_response(self) -> Response {
        match self {
            Ok(v) => v.into_response(),
            Err(e) => e.into_response(),
        }
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        T::describe(op, registry);
        E::describe(op, registry);
    }
}
