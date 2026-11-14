//! The [`App`] builder.
//!
//! An `App` is a *description* of an application: routes, mounted child
//! apps, state, middleware and lifespan hooks. Nothing is materialised until
//! [`App::run`] or [`App::into_router_service`], which validate the whole
//! configuration first and report problems as [`ServerError::Configuration`]
//! instead of panicking.
//!
//! # Mounted applications
//! `app.mount("/api/v1", child)` keeps the child's state and middleware
//! scoped to the child's routes. Child routes are **merged** into the root
//! OpenAPI document with the prefix prepended; child docs settings are
//! ignored. Child lifespan hooks are lifted into the parent (child hooks run
//! after the parent's at startup, before them at shutdown).

use crate::background::{BackgroundTaskLimits, TaskManager};
use crate::body::Body;
use crate::di::{self, Active, DiRegistry, RouteScope, Seed};
use crate::error::{ApiError, ServerError};
use crate::lifespan::{DEFAULT_SHUTDOWN_BUDGET, Lifespan};
use crate::response::IntoResponse;
use crate::routing::{Endpoint, MethodRouter, Route};
use crate::service::RootRouter;
use siderite_openapi::{DocumentBuilder, OpenApi, OpenApiError, ui};
use siderite_orm::{Databases, Db};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Future type used by lifespan hooks.
pub(crate) type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Deferred transformation of the internal router (state, middleware,
/// services). Applied in registration order when the app is built.
pub(crate) type RouterLayer = Box<dyn FnOnce(axum::Router) -> axum::Router + Send>;

/// A startup or shutdown hook.
pub(crate) type LifespanHook = Box<dyn FnOnce() -> BoxFuture<Result<(), ApiError>> + Send>;

/// Application metadata used for OpenAPI generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppMeta {
    /// API title.
    pub title: String,
    /// API version.
    pub version: String,
    /// API description.
    pub description: Option<String>,
}

impl Default for AppMeta {
    fn default() -> Self {
        Self {
            title: "siderite".to_owned(),
            version: "0.1.0".to_owned(),
            description: None,
        }
    }
}

/// Where the generated documentation is served. `None` disables an endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsConfig {
    /// OpenAPI JSON document (default `/openapi.json`).
    pub openapi_url: Option<String>,
    /// Swagger UI (default `/docs`); requires `openapi_url`.
    pub swagger_url: Option<String>,
    /// ReDoc (default `/redoc`); requires `openapi_url`.
    pub redoc_url: Option<String>,
}

impl Default for DocsConfig {
    fn default() -> Self {
        Self {
            openapi_url: Some("/openapi.json".to_owned()),
            swagger_url: Some("/docs".to_owned()),
            redoc_url: Some("/redoc".to_owned()),
        }
    }
}

/// A child app mounted under a prefix.
struct Mount {
    prefix: String,
    app: App,
}

/// An HTTP application: routes, state, middleware, lifespan and metadata.
pub struct App {
    pub(crate) meta: AppMeta,
    pub(crate) docs: DocsConfig,
    pub(crate) routes: Vec<(String, MethodRouter)>,
    mounts: Vec<Mount>,
    /// Values inserted into every routed request (state, ...); the first
    /// registered wins.
    pub(crate) seeds: Vec<Seed>,
    /// Services mounted at prefixes (e.g. static files); not documented.
    pub(crate) services: Vec<RouterLayer>,
    /// Replaces the default 404 problem fallback (e.g. root static files).
    pub(crate) fallback: Option<RouterLayer>,
    pub(crate) startup_hooks: Vec<LifespanHook>,
    pub(crate) shutdown_hooks: Vec<LifespanHook>,
    pub(crate) shutdown_budget: std::time::Duration,
    pub(crate) background_limits: BackgroundTaskLimits,
    pub(crate) server_limits: crate::ServerLimits,
    /// Middleware; the first registered is the outermost.
    pub(crate) middleware: Vec<RouterLayer>,
    /// Dependency-injection configuration.
    pub(crate) di: DiRegistry,
    /// Named databases, exposed to handlers as `State<Databases>`.
    databases: Option<Databases>,
    /// Builder misuse reported by [`validate`](Self::validate) instead of
    /// panicking while the router is built.
    pub(crate) config_errors: Vec<String>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("meta", &self.meta)
            .field(
                "routes",
                &self.routes.iter().map(|(p, _)| p).collect::<Vec<_>>(),
            )
            .field(
                "mounts",
                &self.mounts.iter().map(|m| &m.prefix).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) async fn route_not_found() -> axum::response::Response {
    ApiError::not_found("No route matches the requested path.")
        .into_response()
        .map(Body::into_inner)
}

impl App {
    /// Create an empty application.
    pub fn new() -> Self {
        Self {
            meta: AppMeta::default(),
            docs: DocsConfig::default(),
            routes: Vec::new(),
            mounts: Vec::new(),
            seeds: Vec::new(),
            services: Vec::new(),
            fallback: None,
            startup_hooks: Vec::new(),
            shutdown_hooks: Vec::new(),
            shutdown_budget: DEFAULT_SHUTDOWN_BUDGET,
            background_limits: BackgroundTaskLimits::default(),
            server_limits: crate::ServerLimits::default(),
            middleware: Vec::new(),
            di: DiRegistry::default(),
            databases: None,
            config_errors: Vec::new(),
        }
    }

    /// Set the API title.
    #[must_use]
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.meta.title = title.into();
        self
    }

    /// Set the API version.
    #[must_use]
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.meta.version = version.into();
        self
    }

    /// Set the API description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.meta.description = Some(description.into());
        self
    }

    /// Configure documentation endpoints.
    #[must_use]
    pub fn docs(mut self, docs: DocsConfig) -> Self {
        self.docs = docs;
        self
    }

    /// The application metadata.
    pub fn meta(&self) -> &AppMeta {
        &self.meta
    }

    /// Register `methods` at `path` (use `{param}` for path parameters).
    #[must_use]
    pub fn route(mut self, path: &str, methods: MethodRouter) -> Self {
        self.routes.push((path.to_owned(), methods));
        self
    }

    /// Register routes produced by route macros (see `routes![]`).
    #[must_use]
    pub fn routes(mut self, routes: impl IntoIterator<Item = Route>) -> Self {
        for route in routes {
            self = self.route(route.path, route.router);
        }
        self
    }

    /// Mount `app` under `prefix` (e.g. `/api/v1`); `/` merges its routes
    /// into this app. See the module docs.
    #[must_use]
    pub fn mount(mut self, prefix: &str, app: App) -> Self {
        self.mounts.push(Mount {
            prefix: prefix.trim_end_matches('/').to_owned(),
            app,
        });
        self
    }

    /// Alias of [`App::mount`].
    #[must_use]
    pub fn nest(self, prefix: &str, app: App) -> Self {
        self.mount(prefix, app)
    }

    /// Make `state` available to handlers through [`State<T>`](crate::State).
    ///
    /// Applies to every route of this app regardless of call order.
    #[must_use]
    pub fn with_state<T: Send + Sync + 'static>(mut self, state: T) -> Self {
        let state = Arc::new(state);
        self.seeds
            .push(Arc::new(move |extensions: &mut http::Extensions| {
                extensions.insert(Arc::clone(&state));
            }));
        self
    }

    /// Register `db` under `alias` (`"default"`, `"replica"`, ...).
    ///
    /// Every registered database is available to handlers as
    /// `State<Databases>`; a router set with [`App::databases`] is kept.
    #[must_use]
    pub fn database(mut self, alias: impl Into<String>, db: Db) -> Self {
        let databases = self.databases.take().unwrap_or_default();
        self.databases = Some(databases.with(alias, db));
        self
    }

    /// Replace the whole database registry (aliases and router).
    #[must_use]
    pub fn databases(mut self, databases: Databases) -> Self {
        self.databases = Some(databases);
        self
    }

    /// The registered databases, if any.
    pub fn database_registry(&self) -> Option<&Databases> {
        self.databases.as_ref()
    }

    /// Every documented endpoint as `(full path, endpoint)`, including mounts.
    fn endpoints(&self) -> Vec<(String, &Endpoint)> {
        let mut out: Vec<(String, &Endpoint)> = self
            .routes
            .iter()
            .flat_map(|(path, r)| r.endpoints.iter().map(move |e| (path.clone(), e)))
            .collect();
        for mount in &self.mounts {
            for (path, e) in mount.app.endpoints() {
                let full = if path == "/" && !mount.prefix.is_empty() {
                    mount.prefix.clone()
                } else {
                    format!("{}{path}", mount.prefix)
                };
                out.push((full, e));
            }
        }
        out
    }

    /// Generate the OpenAPI 3.1 document for this app and its mounts.
    ///
    /// # Errors
    /// Returns [`OpenApiError`] on duplicate operations/ids or schema name
    /// conflicts.
    pub fn openapi(&self) -> Result<OpenApi, OpenApiError> {
        let mut builder = DocumentBuilder::new(&self.meta.title, &self.meta.version);
        if let Some(d) = &self.meta.description {
            builder = builder.description(d);
        }
        for (path, endpoint) in self.endpoints() {
            if endpoint.meta.hidden {
                continue;
            }
            if let Some(method) = endpoint.openapi_method() {
                let op = endpoint.operation(builder.registry());
                builder.add_operation(&path, method, op);
            }
        }
        builder.build()
    }

    /// The first builder error recorded by this app or a mounted one.
    fn first_config_error(&self) -> Option<&str> {
        self.config_errors
            .first()
            .map(String::as_str)
            .or_else(|| self.mounts.iter().find_map(|m| m.app.first_config_error()))
    }

    /// Validate route paths and reject duplicate `(path, method)` pairs.
    fn validate(&self) -> Result<(), ServerError> {
        let mut seen: BTreeMap<(String, String), ()> = BTreeMap::new();
        for (path, e) in self.endpoints() {
            if !path.starts_with('/') {
                return Err(ServerError::Configuration(format!(
                    "route path `{path}` must start with `/`"
                )));
            }
            if e.openapi_method().is_none() {
                return Err(ServerError::Configuration(format!(
                    "unsupported HTTP method {} for route {path}",
                    e.method
                )));
            }
            if seen
                .insert((path.clone(), e.method.to_string()), ())
                .is_some()
            {
                return Err(ServerError::Configuration(format!(
                    "duplicate route {} {path}",
                    e.method
                )));
            }
        }
        for (name, url) in [
            ("openapi_url", &self.docs.openapi_url),
            ("swagger_url", &self.docs.swagger_url),
            ("redoc_url", &self.docs.redoc_url),
        ] {
            if let Some(url) = url
                && !url.starts_with('/')
            {
                return Err(ServerError::Configuration(format!(
                    "docs `{name}` `{url}` must start with `/`"
                )));
            }
        }
        if let Some(message) = self.first_config_error() {
            return Err(ServerError::Configuration(message.to_owned()));
        }
        for m in &self.mounts {
            // An empty prefix is a root mount (`mount("/", ..)`); its routes
            // merge into this app, which keeps the fallback.
            if m.prefix.is_empty() && m.app.fallback.is_some() {
                return Err(ServerError::Configuration(
                    "an app mounted at `/` cannot serve root static files; \
                     call `static_files(\"/\", ..)` on the parent app"
                        .to_owned(),
                ));
            }
            if !m.prefix.is_empty() && !m.prefix.starts_with('/') {
                return Err(ServerError::Configuration(format!(
                    "mount prefix `{}` must start with `/`",
                    m.prefix
                )));
            }
        }
        Ok(())
    }

    /// Build the internal router; lifts lifespan hooks out of mounts.
    ///
    /// Layering, innermost first: routes, mounts, services, DI scope, state
    /// and other registered values. The app's own endpoints apply the scope
    /// themselves (see [`RouteScope`]) instead of through a router layer,
    /// which would box and clone every route on every request. The app's
    /// middleware is returned separately so the caller applies it once
    /// everything else (docs, fallback) exists. `parent` is the DI chain of
    /// the enclosing app.
    fn build_router(
        mut self,
        hooks: &mut Lifespan,
        root: bool,
        parent: Option<Active>,
    ) -> (axum::Router, Vec<RouterLayer>, Option<RouterLayer>) {
        hooks.startup.append(&mut self.startup_hooks);
        // Own shutdown hooks come before the children's in the list; shutdown runs
        // the list reversed, so children stop first, mirroring startup.
        hooks.shutdown.append(&mut self.shutdown_hooks);

        let has_background = self.routes.iter().any(|(_, methods)| {
            methods
                .endpoints
                .iter()
                .any(|endpoint| endpoint.background_tasks)
        });
        let mut seeds = std::mem::take(&mut self.seeds);
        if has_background {
            let manager = TaskManager::new(self.background_limits);
            hooks.background.push(manager.clone());
            // Registered last: it used to be the outermost layer.
            seeds.push(Arc::new(move |extensions: &mut http::Extensions| {
                extensions.insert(manager.clone());
            }));
        }
        let install_di = root || !self.di.is_empty();
        let di = std::mem::take(&mut self.di);
        let (scope, inherited) = RouteScope::new(seeds, di, install_di, parent);
        let scope = Arc::new(scope);

        let mut grouped: BTreeMap<String, Vec<Endpoint>> = BTreeMap::new();
        for (path, r) in self.routes {
            grouped.entry(path).or_default().extend(r.endpoints);
        }
        let mut router = axum::Router::new();
        for (path, endpoints) in grouped {
            let mut method_router = endpoints
                .into_iter()
                .filter_map(|e| e.into_axum(&scope))
                .fold(
                    axum::routing::MethodRouter::new(),
                    axum::routing::MethodRouter::merge,
                );
            if !scope.is_noop() {
                // 405 answers went through the scope layer too.
                method_router =
                    method_router.fallback_service(di::method_not_allowed(Arc::clone(&scope)));
            }
            router = router.route(&path, method_router);
        }
        // Everything else gets the scope as a router layer.
        let mut other = axum::Router::new();
        let mut has_other = false;
        for mount in self.mounts {
            let (child, middleware, _) = mount.app.build_router(hooks, false, inherited.clone());
            let child = apply_middleware(child, middleware);
            other = if mount.prefix.is_empty() {
                other.merge(child)
            } else {
                other.nest(&mount.prefix, child)
            };
            has_other = true;
        }
        for service in self.services {
            other = service(other);
            has_other = true;
        }
        // The root app installs its fallback in `build`, after the docs routes.
        let mut fallback = self.fallback;
        if !root && let Some(f) = fallback.take() {
            other = f(other);
            has_other = true;
        }
        if has_other {
            if !scope.is_noop() {
                other = di::install(other, Arc::clone(&scope));
            }
            router = router.merge(other);
        }
        (router, self.middleware, fallback)
    }

    /// Validate and build the root router, including documentation routes.
    pub(crate) fn build_root(mut self) -> Result<(RootRouter, Lifespan), ServerError> {
        self.validate()?;
        if let Some(databases) = self.databases.take() {
            self = self.with_state(databases);
        }
        let openapi = self
            .openapi()
            .map_err(|e| ServerError::Configuration(e.to_string()))?;
        let docs = self.docs.clone();
        let title = self.meta.title.clone();
        let upgrades = crate::server::tasks::TaskOwner::new(self.server_limits.max_websockets);
        let mut hooks = Lifespan {
            shutdown_budget: self.shutdown_budget,
            server: Some(crate::server::ServerOwners::new(self.server_limits)),
            ..Lifespan::default()
        };
        let (mut router, middleware, fallback) = self.build_router(&mut hooks, true, None);
        if let Some(spec_url) = docs.openapi_url {
            let json = serde_json::to_string(&openapi)
                .map_err(|e| ServerError::Configuration(e.to_string()))?;
            // Docs routes sit outside the DI layer, which records the matched
            // route for request logging, so they record it themselves.
            let mut docs_router =
                axum::Router::new().route(&spec_url, static_route("application/json", json));
            if let Some(url) = docs.swagger_url {
                let html = ui::swagger_ui_html(&title, &spec_url);
                docs_router =
                    docs_router.route(&url, static_route("text/html; charset=utf-8", html));
            }
            if let Some(url) = docs.redoc_url {
                let html = ui::redoc_html(&title, &spec_url);
                docs_router =
                    docs_router.route(&url, static_route("text/html; charset=utf-8", html));
            }
            router = router.merge(docs_router.route_layer(axum::middleware::from_fn(
                |req: axum::extract::Request, next: axum::middleware::Next| {
                    let (parts, body) = req.into_parts();
                    crate::middleware::note_matched_path(&parts.extensions);
                    next.run(axum::extract::Request::from_parts(parts, body))
                },
            )));
        }
        let router = match fallback {
            Some(install) => install(router),
            None => router.fallback(route_not_found),
        };
        let router = apply_middleware(router, middleware);
        let values = crate::service::ServerValues {
            upgrades: upgrades.handle(),
            readiness: hooks.server.as_ref().map(|s| s.readiness.clone()),
        };
        hooks.transports.push(upgrades);
        Ok((RootRouter::new(router, values), hooks))
    }

    /// [`build_root`](Self::build_root) as a plain engine router.
    #[cfg(test)]
    pub(crate) fn build(self) -> Result<(axum::Router, Lifespan), ServerError> {
        self.build_root()
            .map(|(root, lifespan)| (root.into_router(), lifespan))
    }

    /// Convert into an in-process `tower::Service` (used by the testkit).
    /// Lifespan hooks are **not** run.
    ///
    /// # Errors
    /// Returns [`ServerError::Configuration`] if the app is misconfigured.
    pub fn into_router_service(self) -> Result<crate::RouterService, ServerError> {
        self.build_root()
            .map(|(router, _)| crate::RouterService::from_root(router))
    }

    /// Like [`App::into_router_service`], but also returns the [`Lifespan`]
    /// so the caller can run startup and shutdown hooks around the service
    /// without binding a socket (used by the testkit).
    ///
    /// # Errors
    /// Returns [`ServerError::Configuration`] if the app is misconfigured.
    pub fn into_service_with_lifespan(
        self,
    ) -> Result<(crate::RouterService, Lifespan), ServerError> {
        self.build_root()
            .map(|(router, lifespan)| (crate::RouterService::from_root(router), lifespan))
    }

    /// Serve until Ctrl-C or Unix SIGTERM, then drain and clean up resources.
    ///
    /// Args:
    ///     addr: Address to bind.
    ///
    /// Returns:
    ///     Success after shutdown and resource cleanup.
    ///
    /// # Errors
    /// Configuration, startup, bind, serving or shutdown errors.
    pub async fn run(self, addr: &str) -> Result<(), ServerError> {
        self.run_until(addr, shutdown_signal()).await
    }

    /// Serve until a caller-supplied shutdown future completes.
    ///
    /// Startup cancellation and bind failure clean initialized resources.
    /// HTTP draining and teardown share one deadline. Connection, stream
    /// and WebSocket workers are cancelled before resource teardown.
    ///
    /// Args:
    ///     addr: Address to bind.
    ///     shutdown: Future requesting graceful termination.
    ///
    /// Returns:
    ///     Success after serving and cleanup finish.
    ///
    /// # Errors
    /// Configuration/startup/I/O errors or exceeded shutdown deadline.
    pub async fn run_until<F>(self, addr: &str, shutdown: F) -> Result<(), ServerError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let budget = self.shutdown_budget;
        let limits = self.server_limits;
        let (router, lifespan) = self.build_root()?;
        let server = lifespan
            .server
            .clone()
            .ok_or_else(|| ServerError::Configuration("missing server owner".to_owned()))?;
        let mut owner = lifespan.supervise()?;
        let mut shutdown = Box::pin(shutdown);
        tokio::select! {
            result = owner.ready() => result?,
            _ = &mut shutdown => return owner.shutdown().await,
        }
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => listener,
            Err(source) => {
                let _ = owner.shutdown().await;
                return Err(ServerError::Bind {
                    addr: addr.to_owned(),
                    source,
                });
            }
        };
        tracing::info!(%addr, "listening");
        let serving = crate::server::serve(listener, router, limits, server, budget, shutdown);
        let (result, deadline) = serving.await;
        let stopped = owner.shutdown_at(deadline).await;
        result?;
        stopped
    }
}

/// Apply middleware so that the first registered layer is the outermost.
fn apply_middleware(router: axum::Router, layers: Vec<RouterLayer>) -> axum::Router {
    layers
        .into_iter()
        .rev()
        .fold(router, |router, layer| layer(router))
}

fn static_route(content_type: &'static str, body: String) -> axum::routing::MethodRouter {
    let body: Arc<str> = body.into();
    axum::routing::get(move || {
        let body = Arc::clone(&body);
        async move {
            let mut resp = axum::response::Response::new(axum::body::Body::from(body.to_string()));
            resp.headers_mut().insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static(content_type),
            );
            resp
        }
    })
}

async fn shutdown_signal() {
    let interrupt = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = interrupt => {}, _ = terminate => {} }
}
