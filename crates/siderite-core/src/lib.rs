//! Core HTTP layer of siderite: application builder, routing, extraction,
//! responses, dependency injection, middleware and RFC 7807 errors.
//!
//! The underlying HTTP engine is an internal implementation detail; the
//! public API consists of siderite-owned types and traits. Every extractor
//! and response type can document itself, so OpenAPI operations are derived
//! from handler signatures.
#![forbid(unsafe_code)]

pub mod app;
pub mod background;
pub mod body;
pub mod di;
pub mod error;
pub mod extract;
pub mod form;
pub mod handler;
pub mod header;
pub mod health;
pub mod lifespan;
pub mod middleware;
pub mod pagination;
pub mod response;
pub mod responses;
pub mod routing;
pub mod security;
mod server;
pub mod service;
pub mod sse;
pub mod state;
pub mod static_files;
pub mod ws;

pub use app::{App, AppMeta, DocsConfig};
pub use background::{
    BackgroundTaskError, BackgroundTaskLimits, BackgroundTasks, Task, TaskId, TaskQueue,
    TaskQueueError,
};
pub use body::{Body, DEFAULT_BODY_LIMIT};
pub use di::{Dependency, DependencyError, Depends, Provided, RequestHead, ResolveContext};
pub use error::{ApiError, ApiResult, BodyError, ServerError};
pub use extract::{FromRequest, FromRequestParts, Path, Query, RawRequest, Request};
pub use form::{DEFAULT_MULTIPART_LIMIT, Form, Multipart, MultipartField};
pub use handler::Handler;
pub use header::{
    Accept, Cookies, Header, NamedHeader, SameSite, SetCookie, UserAgent, WithCookies,
};
pub use lifespan::{Lifespan, ManagedLifespan, Resource};
pub use middleware::{
    BodyLimit, BoxService, Compression, ConcurrencyLimit, ConcurrencyLimitError, ConcurrencyStats,
    Cors, HttpsRedirect, Next, RateLimit, RateLimitError, RequestId, RequestIdLayer,
    RequestLogging, Timeout, TrustedHosts, TrustedProxies, from_fn,
};
pub use middleware::{CsrfOrigin, CsrfOriginError};
pub use pagination::{CursorPage, CursorParams, Page, PageParams};
pub use response::{
    Html, IntoResponse, Json, JsonDump, NoContent, PlainText, Response, WithStatus,
};
pub use responses::{Cached, FileResponse, Redirect, StreamingResponse, WithHeaders};
pub use routing::{
    MethodRouter, OperationMeta, Route, delete, get, head, options, patch, post, put,
};
pub use security::{
    ApiKey, ApiKeyLocation, ApiKeySpec, Authenticate, HttpBasic, HttpBearer, NoScopes,
    OAuth2PasswordBearer, OAuth2PasswordRequestForm, OAuth2Spec, Scopes, Security,
};
pub use server::{Readiness, ServerLimits, ServerPhase};
pub use service::RouterService;
pub use sse::Sse;
pub use state::State;
pub use ws::{CloseFrame, Message, WebSocket, WebSocketResponse, WebSocketUpgrade, WsError};

/// Re-exports of standard HTTP types used in the public API.
pub mod http {
    pub use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
}
