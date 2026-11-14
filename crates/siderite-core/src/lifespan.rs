//! Application lifespan: startup/shutdown hooks and scoped resources.
//!
//! Hooks run in registration order at startup and in **reverse** order at
//! shutdown; hooks of mounted child apps run after the parent's at startup
//! and before them at shutdown. A failing startup hook aborts startup with
//! [`ServerError::Lifespan`] after reverse-order cleanup. At shutdown every
//! hook runs even if one fails; the first error is reported.
//!
//! [`App::lifespan_resource`] is the equivalent of FastAPI's lifespan
//! context manager: a value created at startup, injected with
//! [`Resource<T>`], and cleaned up at shutdown.

use crate::app::{App, BoxFuture, LifespanHook};
use crate::error::{ApiError, ServerError};
use crate::extract::FromRequestParts;
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::time::Duration;
use tokio::time::Instant;

mod managed;
pub use managed::ManagedLifespan;

/// Default budget for request draining and lifespan teardown.
pub const DEFAULT_SHUTDOWN_BUDGET: Duration = Duration::from_secs(30);

use http::StatusCode;
use http::request::Parts;
use std::future::Future;
use std::ops::Deref;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

/// The startup and shutdown hooks collected from an app tree.
pub struct Lifespan {
    pub(crate) startup: Vec<LifespanHook>,
    pub(crate) shutdown: Vec<LifespanHook>,
    pub(crate) shutdown_budget: Duration,
    pub(crate) background: Vec<crate::background::TaskManager>,
    pub(crate) transports: Vec<crate::server::tasks::TaskOwner>,
    pub(crate) server: Option<crate::server::ServerOwners>,
}

impl Default for Lifespan {
    fn default() -> Self {
        Self {
            startup: Vec::new(),
            shutdown: Vec::new(),
            shutdown_budget: DEFAULT_SHUTDOWN_BUDGET,
            background: Vec::new(),
            transports: Vec::new(),
            server: None,
        }
    }
}

impl std::fmt::Debug for Lifespan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lifespan")
            .field("startup_hooks", &self.startup.len())
            .field("shutdown_hooks", &self.shutdown.len())
            .finish()
    }
}

impl Lifespan {
    /// Start a cleanup supervisor without waiting for initialization.
    ///
    /// Args:
    ///     self: The hooks whose ownership transfers to the supervisor.
    ///
    /// Returns:
    ///     An owner used to await readiness and request shutdown.
    ///
    /// # Errors
    /// Missing Tokio runtime. Use this API for cancellation-safe ownership;
    /// initializers must clean their own work until they return successfully.
    pub fn supervise(self) -> Result<ManagedLifespan, ServerError> {
        ManagedLifespan::new(self)
    }

    /// Run startup and unwind successful initialization on error or panic.
    ///
    /// Args:
    ///     self: Lifespan retained by the caller.
    ///
    /// Returns:
    ///     Success, or the original startup error after cleanup.
    ///
    /// # Errors
    /// Hook or cleanup failure. Direct callers must not cancel this method
    /// without subsequently calling shutdown; prefer [`Self::supervise`].
    pub async fn startup(&mut self) -> Result<(), ServerError> {
        let result = self.start_hooks().await;
        if result.is_err() {
            let _ = self.shutdown().await;
        }
        result
    }

    async fn start_hooks(&mut self) -> Result<(), ServerError> {
        for hook in std::mem::take(&mut self.startup) {
            call_hook(hook, "startup")
                .await
                .map_err(ServerError::Lifespan)?;
        }
        if let Some(server) = &self.server {
            server.readiness.ready();
        }
        Ok(())
    }

    /// Run every shutdown hook in reverse order within the configured budget.
    ///
    /// Args:
    ///     self: Lifespan whose remaining teardown hooks are consumed.
    ///
    /// Returns:
    ///     Success, or the first failure after all cleanup attempts.
    ///
    /// # Errors
    /// Hook failure/panic or deadline expiry. Use a supervisor to shield this
    /// operation from caller cancellation. Hooks must cooperate with polling.
    pub async fn shutdown(&mut self) -> Result<(), ServerError> {
        self.shutdown_at(Instant::now() + self.shutdown_budget)
            .await
    }

    async fn shutdown_at(&mut self, deadline: Instant) -> Result<(), ServerError> {
        let mut first_error = None;
        let server = self.server.take();
        if let Some(server) = &server
            && !server.stop_at(deadline).await
        {
            first_error.get_or_insert(ServerError::ShutdownTimeout);
        }
        for owner in std::mem::take(&mut self.transports) {
            if !owner.stop_at(deadline).await {
                first_error.get_or_insert(ServerError::ShutdownTimeout);
            }
        }
        for manager in std::mem::take(&mut self.background).into_iter().rev() {
            if let Err(error) = manager.shutdown_at(deadline).await {
                tracing::error!(%error, "background shutdown failed");
                first_error.get_or_insert(error);
            }
        }
        while let Some(hook) = self.shutdown.pop() {
            let result = tokio::time::timeout_at(deadline, call_hook(hook, "shutdown")).await;
            let error = match result {
                Ok(Ok(())) => continue,
                Ok(Err(error)) => ServerError::Lifespan(error),
                Err(_) => ServerError::ShutdownTimeout,
            };
            tracing::error!(%error, "shutdown hook failed");
            first_error.get_or_insert(error);
        }
        if let Some(server) = server {
            server.readiness.stopped();
        }
        first_error.map_or(Ok(()), Err)
    }
}

async fn call_hook(hook: LifespanHook, phase: &str) -> Result<(), ApiError> {
    match AssertUnwindSafe(async move { hook().await })
        .catch_unwind()
        .await
    {
        Ok(result) => result,
        Err(_) => {
            tracing::error!(phase, "lifespan hook panicked");
            Err(ApiError::internal(format!("{phase} hook panicked")))
        }
    }
}

/// Handler argument giving access to a value created by
/// [`App::lifespan_resource`]. Responds `503` until startup has completed
/// (and again after shutdown).
#[derive(Debug)]
pub struct Resource<T>(pub Arc<T>);

impl<T> Clone for Resource<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Deref for Resource<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

/// Holds the teardown future between startup and shutdown.
type TeardownCell = Mutex<Option<BoxFuture<Result<(), ApiError>>>>;

/// Shared cell installed as a request extension; filled at startup.
struct Slot<T>(Arc<RwLock<Option<Arc<T>>>>);

impl<T> Clone for Slot<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: Send + Sync + 'static> FromRequestParts for Resource<T> {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        let slot = parts.extensions.get::<Slot<T>>().ok_or_else(|| {
            ApiError::internal(format!(
                "no lifespan resource of type `{}` registered",
                std::any::type_name::<T>()
            ))
        })?;
        let value = slot
            .0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        value.map(Resource).ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "The service is not ready yet.",
            )
        })
    }
}

impl App {
    /// Set the total graceful-shutdown budget, including resource teardown.
    ///
    /// Args:
    ///     budget: Positive, representable duration (default: 30 seconds).
    ///
    /// Returns:
    ///     The configured application; invalid budgets fail validation.
    #[must_use]
    pub fn shutdown_timeout(mut self, budget: Duration) -> Self {
        if budget.is_zero() || std::time::Instant::now().checked_add(budget).is_none() {
            self.config_errors.push("invalid shutdown timeout".into());
        } else {
            self.shutdown_budget = budget;
        }
        self
    }

    /// Run `hook` at startup (before the socket is bound).
    #[must_use]
    pub fn on_startup<F, Fut>(mut self, hook: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), ApiError>> + Send + 'static,
    {
        self.startup_hooks.push(boxed(hook));
        self
    }

    /// Run `hook` at shutdown (hooks run in reverse registration order).
    #[must_use]
    pub fn on_shutdown<F, Fut>(mut self, hook: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), ApiError>> + Send + 'static,
    {
        self.shutdown_hooks.push(boxed(hook));
        self
    }

    /// Create a resource at startup and tear it down at shutdown.
    ///
    /// `init` returns the value and a *teardown future* (which may capture
    /// clones of whatever it needs to close). Handlers receive the value via
    /// [`Resource<T>`]. Resources are torn down in reverse registration order.
    #[must_use]
    pub fn lifespan_resource<T, F, Fut, C>(mut self, init: F) -> Self
    where
        T: Send + Sync + 'static,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(T, C), ApiError>> + Send + 'static,
        C: Future<Output = Result<(), ApiError>> + Send + 'static,
    {
        let slot: Slot<T> = Slot(Arc::default());
        let teardown: Arc<TeardownCell> = Arc::default();
        let (start_slot, start_teardown) = (slot.clone(), Arc::clone(&teardown));
        self.startup_hooks.push(boxed(move || async move {
            let (value, close) = init().await?;
            *start_slot.0.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(value));
            let close: BoxFuture<Result<(), ApiError>> = Box::pin(close);
            *start_teardown
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(close);
            Ok(())
        }));
        let stop_slot = slot.clone();
        self.shutdown_hooks.push(boxed(move || async move {
            stop_slot
                .0
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            let close = teardown
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            match close {
                Some(close) => close.await,
                None => Ok(()),
            }
        }));
        self.seeds.push(std::sync::Arc::new(
            move |extensions: &mut http::Extensions| {
                extensions.insert(slot.clone());
            },
        ));
        self
    }
}

fn boxed<F, Fut>(hook: F) -> LifespanHook
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), ApiError>> + Send + 'static,
{
    Box::new(move || Box::pin(hook()))
}
