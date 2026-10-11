//! Opt-in readiness with application checks:
//! `App::routes([readiness("/readyz", checks)])`.
//!
//! The route answers `200` only while the server is Ready and every
//! registered check passes, `503` otherwise. Checks are bounded so probe
//! traffic cannot become load: each has a timeout, at most
//! [`ReadinessChecks::concurrency`] run at once, and one evaluation is
//! shared by every request until it is older than the cache period
//! (concurrent probes wait for the evaluation in progress instead of
//! starting their own). Checks reuse the application's existing clients
//! and pools. The public body names each check with `ok`, `failed` or
//! `timeout` only; failure details go to the log at `warn` level.

use crate::http::StatusCode;
use crate::responses::WithHeaders;
use crate::{Json, MethodRouter, Readiness, Route, WithStatus, get};
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use serde::Serialize;
use serde_json::json;
use siderite_orm::Db;
use siderite_validation::{Dump, Schema, SchemaObject, SchemaRegistry};
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// What one check failing means for the log; never sent to clients.
pub type CheckError = String;

/// What a check returns.
pub type CheckFuture = BoxFuture<'static, Result<(), CheckError>>;

type CheckFn = Arc<dyn Fn() -> CheckFuture + Send + Sync>;

/// The outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    /// It passed.
    Ok,
    /// It returned an error.
    Failed,
    /// It did not finish within its timeout.
    Timeout,
}

/// The readiness answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadyReport {
    /// `ready` or `unavailable`.
    pub status: &'static str,
    /// Each check's outcome, by name; empty while the server is not
    /// Ready (no check runs then).
    pub checks: BTreeMap<String, CheckStatus>,
}

impl ReadyReport {
    fn is_ready(&self) -> bool {
        self.status == "ready"
    }
}

impl Dump for ReadyReport {}

impl Schema for ReadyReport {
    fn schema_name() -> Option<&'static str> {
        Some("ReadyReport")
    }

    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        let outcome = json!({ "enum": ["ok", "failed", "timeout"] });
        let properties = json!({
            "status": { "enum": ["ready", "unavailable"] },
            "checks": { "type": "object", "additionalProperties": outcome },
        });
        SchemaObject::of_type("object")
            .with("properties", properties)
            .with("required", json!(["status", "checks"]))
    }
}

/// Named readiness checks and their bounds.
#[derive(Clone)]
pub struct ReadinessChecks {
    checks: Vec<(String, CheckFn)>,
    timeout: Duration,
    concurrency: usize,
    cache: Duration,
    last: Arc<Mutex<Option<(Instant, ReadyReport)>>>,
}

impl std::fmt::Debug for ReadinessChecks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names = self.checks.iter().map(|(n, _)| n.as_str());
        let names: Vec<&str> = names.collect();
        f.debug_struct("ReadinessChecks")
            .field("checks", &names)
            .field("timeout", &self.timeout)
            .field("concurrency", &self.concurrency)
            .field("cache", &self.cache)
            .finish_non_exhaustive()
    }
}

impl Default for ReadinessChecks {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadinessChecks {
    /// No checks; a 2 s timeout, 4 at a time, results kept for 1 s.
    pub fn new() -> Self {
        Self {
            checks: Vec::new(),
            timeout: Duration::from_secs(2),
            concurrency: 4,
            cache: Duration::from_secs(1),
            last: Arc::default(),
        }
    }

    /// Add a check named `name` (shown to clients, so keep it generic:
    /// `database`, not a host name).
    ///
    /// Args:
    ///   name: The check's public name.
    ///   check: Returns `Ok(())` when the dependency is usable; the error
    ///     text is logged, never returned.
    #[must_use]
    pub fn check<F, Fut>(mut self, name: impl Into<String>, check: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), CheckError>> + Send + 'static,
    {
        let run: CheckFn = Arc::new(move || Box::pin(check()));
        self.checks.push((name.into(), run));
        self
    }

    /// Give each check at most `timeout` (default 2 s).
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run at most `limit` checks at once (default 4, at least 1).
    #[must_use]
    pub fn concurrency(mut self, limit: usize) -> Self {
        self.concurrency = limit.max(1);
        self
    }

    /// Reuse an evaluation for `period` (default 1 s); zero evaluates on
    /// every request, still one at a time.
    #[must_use]
    pub fn cache(mut self, period: Duration) -> Self {
        self.cache = period;
        self
    }

    /// The report for a server in `ready` state, evaluating the checks
    /// unless a fresh enough evaluation exists.
    pub async fn report(&self, ready: bool) -> ReadyReport {
        if !ready {
            return ReadyReport {
                status: "unavailable",
                checks: BTreeMap::new(),
            };
        }
        // Holding the lock while evaluating makes concurrent probes wait
        // for this evaluation instead of starting their own.
        let mut last = self.last.lock().await;
        if let Some((at, report)) = last.as_ref()
            && at.elapsed() < self.cache
        {
            return report.clone();
        }
        let report = self.evaluate().await;
        *last = Some((Instant::now(), report.clone()));
        report
    }

    async fn evaluate(&self) -> ReadyReport {
        let timeout = self.timeout;
        // Owned, boxed futures: the stream holds no borrow of `self`.
        let runs: Vec<BoxFuture<'static, (String, CheckStatus)>> = self
            .checks
            .iter()
            .map(|(name, check)| run(name.clone(), Arc::clone(check), timeout))
            .collect();
        let stream = futures_util::stream::iter(runs);
        let running = stream.buffer_unordered(self.concurrency);
        let checks: BTreeMap<String, CheckStatus> = running.collect().await;
        let ok = checks.values().all(|s| *s == CheckStatus::Ok);
        ReadyReport {
            status: if ok { "ready" } else { "unavailable" },
            checks,
        }
    }
}

/// Run one check within `timeout`, logging why it did not pass.
fn run(
    name: String,
    check: CheckFn,
    timeout: Duration,
) -> BoxFuture<'static, (String, CheckStatus)> {
    Box::pin(async move {
        let status = match tokio::time::timeout(timeout, check()).await {
            Ok(Ok(())) => CheckStatus::Ok,
            Ok(Err(error)) => {
                tracing::warn!(check = %name, %error, "check failed");
                CheckStatus::Failed
            }
            Err(_) => {
                tracing::warn!(check = %name, "check timed out");
                CheckStatus::Timeout
            }
        };
        (name, status)
    })
}

type Answer = WithHeaders<WithStatus<Json<ReadyReport>>>;

const CACHE: &str = "cache-control";
const NO_STORE: &str = "no-store";

/// The readiness handler for `checks`, documented under the `health` tag
/// with the operation ID `readiness`.
pub fn readiness_router(checks: ReadinessChecks) -> MethodRouter {
    let checks = Arc::new(checks);
    let handler = move |server: Readiness| {
        let checks = Arc::clone(&checks);
        async move {
            let report = checks.report(server.is_ready()).await;
            let status = if report.is_ready() {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            let body = WithStatus(status, Json(report));
            let answer: Answer = WithHeaders::new(body);
            answer.header(CACHE, NO_STORE)
        }
    };
    get(handler)
        .operation_id("readiness")
        .tag("health")
        .summary("Whether the app is ready to receive traffic.")
        .response_model::<ReadyReport>()
}

/// A `GET path` readiness route running `checks`.
///
/// Args:
///   path: Where to answer, for example `/readyz`.
///   checks: The checks and their bounds.
///
/// Returns:
///   The route; nothing is registered until the app adds it.
pub fn readiness(path: &'static str, checks: ReadinessChecks) -> Route {
    Route::new(path, readiness_router(checks))
}

/// A check that the database answers `SELECT 1` through `db`'s existing
/// pool. SQL backends only: on MongoDB or Redis it fails, so write a
/// check with the client the app already has.
pub fn sql_check(db: Db) -> impl Fn() -> CheckFuture + Send + Sync {
    move || {
        let db = db.clone();
        Box::pin(async move {
            db.raw_sql("SELECT 1", Vec::new())
                .await
                .map(drop)
                .map_err(|e| e.to_string())
        })
    }
}
