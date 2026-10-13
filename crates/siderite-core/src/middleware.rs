//! Middleware: tower layers over the whole application, plus batteries.
//!
//! [`App::layer`] accepts any `tower::Layer` over [`BoxService`], the
//! application as a `Service<http::Request<Body>>`. [`from_fn`] builds one
//! from an async function.
//!
//! # Ordering
//! **The first layer registered is the outermost**: it sees the request first
//! and the response last, exactly like reading `ServiceBuilder` top to bottom.
//! `app.request_id().request_logging().cors(..)` runs request-id, then
//! logging, then CORS, then the routes. Registration order relative to
//! `with_state`, `provide`, ... does not matter: middleware always wraps
//! those. Middleware of the root app also wraps 404s and the documentation
//! endpoints; middleware of a mounted child wraps only that child's routes.
//!
//! # Errors
//! Rejections produced by built-in middleware are RFC 7807
//! `application/problem+json` documents ([`ApiError`](crate::ApiError)).
//!
//! # Built-ins
//! [`Cors`], [`Compression`], [`CsrfOrigin`], [`TrustedHosts`],
//! [`HttpsRedirect`], [`RequestIdLayer`] (+ [`RequestId`] extractor),
//! [`RequestLogging`], [`Timeout`], [`ConcurrencyLimit`], [`BodyLimit`] and
//! [`RateLimit`], each with a same-named convenience method on [`App`].

mod adapt;
mod cors;
mod csrf;
mod hosts;
mod limits;
mod observe;

use crate::app::App;
use crate::extract::Request;
use crate::response::Response;
use std::convert::Infallible;
use std::time::Duration;
use tower::{Layer, Service};

pub use adapt::{BoxService, FromFnLayer, Next, from_fn};
pub use cors::{Compression, Cors};
pub use csrf::{CsrfOrigin, CsrfOriginError};
pub use hosts::{HttpsRedirect, TrustedHosts};
mod concurrency;
pub use concurrency::{ConcurrencyLimit, ConcurrencyLimitError, ConcurrencyStats};
pub use limits::{BodyLimit, Timeout};
mod proxy;
mod rate_limit;
pub use observe::{RequestId, RequestIdLayer, RequestLogging};
pub use proxy::{ProxyContext, ProxyError, TrustedProxies};
pub use rate_limit::{RateLimit, RateLimitError};

pub(crate) use observe::note_matched_path;

impl App {
    /// Wrap the application in `layer`. See the [module docs](self) for the
    /// ordering rules.
    #[must_use]
    pub fn layer<L>(mut self, layer: L) -> Self
    where
        L: Layer<BoxService> + Clone + Send + Sync + 'static,
        L::Service: Service<Request, Response = Response, Error = Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
        <L::Service as Service<Request>>::Future: Send + 'static,
    {
        self.middleware
            .push(Box::new(move |router| adapt::apply_layer(&layer, router)));
        self
    }

    /// Add [`Cors`] middleware.
    #[must_use]
    pub fn cors(self, cors: Cors) -> Self {
        self.layer(cors)
    }

    /// Add [`Compression`] middleware.
    #[must_use]
    pub fn compression(self, compression: Compression) -> Self {
        self.layer(compression)
    }

    /// Add [`CsrfOrigin`] middleware.
    #[must_use]
    pub fn csrf_origin(self, csrf: CsrfOrigin) -> Self {
        self.layer(csrf)
    }

    /// Add [`TrustedHosts`] middleware.
    #[must_use]
    pub fn trusted_hosts(self, hosts: TrustedHosts) -> Self {
        self.layer(hosts)
    }

    /// Add [`HttpsRedirect`] middleware.
    #[must_use]
    pub fn https_redirect(self, redirect: HttpsRedirect) -> Self {
        self.layer(redirect)
    }

    /// Add request-id middleware with default settings.
    #[must_use]
    pub fn request_id(self) -> Self {
        self.layer(RequestIdLayer::new())
    }

    /// Add [`RequestLogging`] middleware.
    #[must_use]
    pub fn request_logging(self) -> Self {
        self.layer(RequestLogging::new())
    }

    /// Add a [`Timeout`] (504 by default).
    #[must_use]
    pub fn timeout(self, limit: Duration) -> Self {
        self.layer(Timeout::new(limit))
    }

    /// Add a [`ConcurrencyLimit`].
    #[must_use]
    pub fn concurrency_limit(mut self, max: usize) -> Self {
        let limit = ConcurrencyLimit::new(max);
        if let Err(error) = limit.validate() {
            self.config_errors.push(error.to_string());
        }
        self.layer(limit)
    }

    /// Add a [`BodyLimit`] of `max_bytes`.
    #[must_use]
    pub fn body_limit(self, max_bytes: usize) -> Self {
        self.layer(BodyLimit::new(max_bytes))
    }

    /// Add a [`RateLimit`].
    #[must_use]
    pub fn rate_limit(mut self, limit: RateLimit) -> Self {
        if !limit.valid() {
            self.config_errors
                .push("invalid rate limiter configuration".into());
        }
        self.layer(limit)
    }
}
