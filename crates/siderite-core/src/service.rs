//! Opaque tower service produced from an [`App`](crate::App).

use crate::body::Body;
use crate::server::Readiness;
use crate::server::tasks::TaskHandle;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::Service;

/// A cloneable `tower::Service` handling `http::Request<Body>`; used for
/// in-process testing without opening a socket.
#[derive(Debug, Clone)]
pub struct RouterService {
    inner: RootRouter,
}

impl RouterService {
    pub(crate) fn new(inner: axum::Router) -> Self {
        Self {
            inner: RootRouter::from(inner),
        }
    }

    pub(crate) fn from_root(inner: RootRouter) -> Self {
        Self { inner }
    }
}

impl Service<http::Request<Body>> for RouterService {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<Body>) -> Self::Future {
        let fut = self.inner.call(req.map(Body::into_inner));
        Box::pin(async move {
            match fut.await {
                Ok(response) => Ok(response.map(Body::from_inner)),
                Err(never) => match never {},
            }
        })
    }
}

/// Server-wide values every request carries, inserted as one extension.
#[derive(Clone)]
pub(crate) struct ServerValues {
    /// Admission for WebSocket upgrades.
    pub(crate) upgrades: TaskHandle,
    /// Lifecycle of the running server, when there is one.
    pub(crate) readiness: Option<Readiness>,
}

/// The root router plus the server-wide values it hands to each request.
///
/// The values are inserted here rather than by router layers: an engine
/// router layer wraps every route separately and costs boxed clones of the
/// whole route stack on each request.
#[derive(Clone)]
pub(crate) struct RootRouter {
    router: axum::Router,
    values: Option<ServerValues>,
}

impl std::fmt::Debug for RootRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootRouter")
            .field("router", &self.router)
            .finish_non_exhaustive()
    }
}

impl From<axum::Router> for RootRouter {
    fn from(router: axum::Router) -> Self {
        Self {
            router,
            values: None,
        }
    }
}

impl RootRouter {
    pub(crate) fn new(router: axum::Router, values: ServerValues) -> Self {
        Self {
            router,
            values: Some(values),
        }
    }

    /// The bare router with the values installed as a fallback wrapper, for
    /// callers that need an engine router (tests serving it directly).
    #[cfg(test)]
    pub(crate) fn into_router(self) -> axum::Router {
        if self.values.is_none() {
            return self.router;
        }
        axum::Router::new().fallback_service(self)
    }
}

impl Service<axum::extract::Request> for RootRouter {
    type Response = axum::response::Response;
    type Error = Infallible;
    type Future = axum::routing::future::RouteFuture<Infallible>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Service::<axum::extract::Request>::poll_ready(&mut self.router, cx)
    }

    fn call(&mut self, mut req: axum::extract::Request) -> Self::Future {
        if let Some(values) = &self.values {
            req.extensions_mut().insert(values.clone());
        }
        self.router.call(req)
    }
}
