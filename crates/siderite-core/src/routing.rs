//! Method routers and per-operation metadata.
//!
//! ```ignore
//! App::new().route(
//!     "/users",
//!     post(create_user).status(201).summary("Create a user").tag("users")
//!         .get(list_users).tag("users"),
//! )
//! ```
//!
//! Metadata methods (`summary`, `tag`, `status`, ...) apply to the **most
//! recently added** method. Route macros (`#[post(...)]`) expand to exactly
//! these calls.

use crate::background::RequestGuard;
use crate::body::Body;
use crate::di::{RouteScope, ScopeOwner};
use crate::extract::Request;
use crate::handler::Handler;
use crate::middleware::BoxService;
use crate::response::{IntoResponse, Response, SUCCESS};
use http::{Method, StatusCode};
use siderite_openapi::{HttpMethod, Operation, Schema, SchemaRegistry};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service, ServiceExt};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Type-erased handler.
pub(crate) type BoxedHandler = Arc<dyn Fn(Request) -> BoxFuture<Response> + Send + Sync>;

/// Function documenting an operation.
pub(crate) type DescribeFn = fn(&mut Operation, &mut SchemaRegistry);

/// Static metadata attached to one operation.
#[derive(Debug, Clone, Default)]
pub struct OperationMeta {
    /// One-line summary.
    pub summary: Option<String>,
    /// Longer description.
    pub description: Option<String>,
    /// Tags.
    pub tags: Vec<String>,
    /// Operation id.
    pub operation_id: Option<String>,
    /// Deprecated flag.
    pub deprecated: bool,
    /// Success status override (applies at runtime and in docs).
    pub status: Option<StatusCode>,
    /// Omit from the OpenAPI document.
    pub hidden: bool,
    pub(crate) response_model: Option<DescribeFn>,
}

impl OperationMeta {
    /// Apply metadata on top of the signature-derived operation.
    pub(crate) fn apply(&self, op: &mut Operation, registry: &mut SchemaRegistry) {
        if let Some(model) = self.response_model {
            model(op, registry);
        }
        if let Some(status) = self.status
            && status.as_u16() != 200
        {
            op.remap_response(SUCCESS, status.as_str());
        }
        op.summary.clone_from(&self.summary);
        op.description.clone_from(&self.description);
        op.tags.clone_from(&self.tags);
        op.operation_id.clone_from(&self.operation_id);
        op.deprecated = self.deprecated;
    }
}

/// One method on one path.
#[derive(Clone)]
pub(crate) struct Endpoint {
    pub(crate) method: Method,
    pub(crate) handler: BoxedHandler,
    pub(crate) describe: DescribeFn,
    pub(crate) meta: OperationMeta,
    pub(crate) background_tasks: bool,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("method", &self.method)
            .field("meta", &self.meta)
            .finish_non_exhaustive()
    }
}

impl Endpoint {
    fn new<H: Handler<T>, T: 'static>(method: Method, handler: H) -> Self {
        let boxed: BoxedHandler = Arc::new(move |req| Box::pin(handler.clone().call(req)));
        Self {
            method,
            handler: boxed,
            describe: H::describe,
            meta: OperationMeta::default(),
            background_tasks: H::BACKGROUND_TASKS,
        }
    }

    /// Build the operation document.
    pub(crate) fn operation(&self, registry: &mut SchemaRegistry) -> Operation {
        let mut op = Operation::default();
        (self.describe)(&mut op, registry);
        self.meta.apply(&mut op, registry);
        op
    }

    /// OpenAPI method, if representable.
    pub(crate) fn openapi_method(&self) -> Option<HttpMethod> {
        Some(match self.method {
            Method::GET => HttpMethod::Get,
            Method::POST => HttpMethod::Post,
            Method::PUT => HttpMethod::Put,
            Method::PATCH => HttpMethod::Patch,
            Method::DELETE => HttpMethod::Delete,
            Method::HEAD => HttpMethod::Head,
            Method::OPTIONS => HttpMethod::Options,
            Method::TRACE => HttpMethod::Trace,
            _ => return None,
        })
    }

    /// Adapt to the internal router, applying the status override and the
    /// app's request `scope` (state, DI) without an extra router layer.
    pub(crate) fn into_axum(self, scope: &Arc<RouteScope>) -> Option<axum::routing::MethodRouter> {
        use axum::routing::MethodFilter;
        let filter = match self.method {
            Method::GET => MethodFilter::GET,
            Method::POST => MethodFilter::POST,
            Method::PUT => MethodFilter::PUT,
            Method::PATCH => MethodFilter::PATCH,
            Method::DELETE => MethodFilter::DELETE,
            Method::HEAD => MethodFilter::HEAD,
            Method::OPTIONS => MethodFilter::OPTIONS,
            Method::TRACE => MethodFilter::TRACE,
            _ => return None,
        };
        let service = EndpointService {
            handler: self.handler,
            status: self.meta.status,
            background: self.background_tasks,
            scope: Arc::clone(scope),
        };
        Some(axum::routing::on_service(filter, service))
    }
}

/// One endpoint as the internal router sees it.
#[derive(Clone)]
struct EndpointService {
    handler: BoxedHandler,
    status: Option<StatusCode>,
    background: bool,
    scope: Arc<RouteScope>,
}

impl EndpointService {
    /// The handler's response with the status override applied, after
    /// background tasks were handed over.
    fn finish(
        status: Option<StatusCode>,
        guard: Option<RequestGuard>,
        mut response: Response,
    ) -> Response {
        if let Some(status) = status
            && response.status() == StatusCode::OK
        {
            *response.status_mut() = status;
        }
        if let Some(guard) = guard {
            guard.finish(response.status());
        }
        response
    }
}

impl Service<axum::extract::Request> for EndpointService {
    type Response = axum::response::Response;
    type Error = Infallible;
    type Future = EndpointFuture;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: axum::extract::Request) -> EndpointFuture {
        let mut req = req.map(Body::from_inner);
        let owner = self.scope.enter(req.extensions_mut());
        if self.scope.has_globals() {
            let this = self.clone();
            let inner = Box::pin(async move {
                let (mut parts, body) = req.into_parts();
                if let Err(err) = this.scope.run_globals(&mut parts).await {
                    return err.into_response();
                }
                let mut req = Request::from_parts(parts, body);
                let guard = this
                    .background
                    .then(|| RequestGuard::install(req.extensions_mut()));
                let response = (this.handler)(req).await;
                Self::finish(this.status, guard, response)
            });
            return EndpointFuture {
                inner,
                status: None,
                guard: None,
                owner: Some(owner),
            };
        }
        let guard = self
            .background
            .then(|| RequestGuard::install(req.extensions_mut()));
        EndpointFuture {
            inner: (self.handler)(req),
            status: self.status,
            guard,
            owner: Some(owner),
        }
    }
}

/// Response future of [`EndpointService`]: the handler's boxed future plus
/// the post-processing done once it completes.
pub(crate) struct EndpointFuture {
    inner: BoxFuture<Response>,
    status: Option<StatusCode>,
    guard: Option<RequestGuard>,
    owner: Option<ScopeOwner>,
}

impl Future for EndpointFuture {
    type Output = Result<axum::response::Response, Infallible>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let response = std::task::ready!(this.inner.as_mut().poll(cx));
        let response = EndpointService::finish(this.status, this.guard.take(), response);
        if let Some(owner) = this.owner.take() {
            owner.finish();
        }
        Poll::Ready(Ok(response.map(Body::into_inner)))
    }
}

/// Maps HTTP methods to handlers for a single path.
#[derive(Debug, Clone, Default)]
pub struct MethodRouter {
    pub(crate) endpoints: Vec<Endpoint>,
}

macro_rules! method_routes {
    ($($name:ident => $method:ident),+ $(,)?) => {
        $(
            #[doc = concat!("Route `", stringify!($method), "` requests to `handler`.")]
            pub fn $name<H: Handler<T>, T: 'static>(handler: H) -> MethodRouter {
                MethodRouter::default().$name(handler)
            }
        )+

        impl MethodRouter {
            $(
                #[doc = concat!("Additionally route `", stringify!($method), "` requests to `handler`.")]
                #[must_use]
                pub fn $name<H: Handler<T>, T: 'static>(self, handler: H) -> Self {
                    self.on(Method::$method, handler)
                }
            )+
        }
    };
}

method_routes! {
    get => GET,
    post => POST,
    put => PUT,
    patch => PATCH,
    delete => DELETE,
    head => HEAD,
    options => OPTIONS,
}

impl MethodRouter {
    /// Route `method` to `handler`. Methods outside GET, POST, PUT, PATCH,
    /// DELETE, HEAD, OPTIONS and TRACE are rejected when the app is built.
    #[must_use]
    pub fn on<H: Handler<T>, T: 'static>(mut self, method: Method, handler: H) -> Self {
        self.endpoints.push(Endpoint::new(method, handler));
        self
    }

    fn last(mut self, f: impl FnOnce(&mut OperationMeta)) -> Self {
        if let Some(e) = self.endpoints.last_mut() {
            f(&mut e.meta);
        }
        self
    }

    /// Wrap every handler added so far in `layer`.
    ///
    /// The layer runs after routing and only for this path, so it can be
    /// used for per-route middleware such as `siderite_cache::RouteCache`.
    /// Handlers added after this call are not wrapped.
    #[must_use]
    pub fn layer<L>(mut self, layer: L) -> Self
    where
        L: Layer<BoxService>,
        L::Service: Service<Request, Response = Response, Error = Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
        <L::Service as Service<Request>>::Future: Send + 'static,
    {
        for endpoint in &mut self.endpoints {
            let handler = Arc::clone(&endpoint.handler);
            let inner = BoxService::new(tower::service_fn(move |req: Request| {
                let fut = handler(req);
                async move { Ok::<_, Infallible>(fut.await) }
            }));
            let service = layer.layer(inner);
            endpoint.handler = Arc::new(move |req| {
                let service = service.clone();
                Box::pin(async move {
                    match service.oneshot(req).await {
                        Ok(response) => response,
                        Err(never) => match never {},
                    }
                })
            });
        }
        self
    }

    /// Set the summary.
    #[must_use]
    pub fn summary(self, summary: impl Into<String>) -> Self {
        let summary = summary.into();
        self.last(|m| m.summary = Some(summary))
    }

    /// Set the description.
    #[must_use]
    pub fn description(self, description: impl Into<String>) -> Self {
        let description = description.into();
        self.last(|m| m.description = Some(description))
    }

    /// Add a tag.
    #[must_use]
    pub fn tag(self, tag: impl Into<String>) -> Self {
        let tag = tag.into();
        self.last(|m| m.tags.push(tag))
    }

    /// Set the operation id.
    #[must_use]
    pub fn operation_id(self, id: impl Into<String>) -> Self {
        let id = id.into();
        self.last(|m| m.operation_id = Some(id))
    }

    /// Mark deprecated.
    #[must_use]
    pub fn deprecated(self) -> Self {
        self.last(|m| m.deprecated = true)
    }

    /// Hide from the OpenAPI document.
    #[must_use]
    pub fn hidden(self) -> Self {
        self.last(|m| m.hidden = true)
    }

    /// Success status: replaces a `200 OK` produced by the handler at
    /// runtime, and documents the success response under this code.
    #[must_use]
    pub fn status(self, status: StatusCode) -> Self {
        self.last(|m| m.status = Some(status))
    }

    /// Document the success response body as `T` (`application/json`),
    /// independent of the handler's return type.
    #[must_use]
    pub fn response_model<T: Schema + 'static>(self) -> Self {
        fn describe<T: Schema + 'static>(op: &mut Operation, registry: &mut SchemaRegistry) {
            let schema = registry.subschema::<T>();
            op.add_response(
                SUCCESS,
                "Successful Response",
                Some(("application/json", schema)),
            );
        }
        self.last(|m| m.response_model = Some(describe::<T>))
    }
}

/// A path plus its method router; produced by route macros and `routes![]`.
#[derive(Debug, Clone)]
pub struct Route {
    /// Path template.
    pub path: &'static str,
    /// Handlers.
    pub router: MethodRouter,
}

impl Route {
    /// Pair a path with a router.
    pub fn new(path: &'static str, router: MethodRouter) -> Self {
        Self { path, router }
    }
}
