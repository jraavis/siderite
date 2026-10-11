//! Test-only backend wrapper that kills the process at a chosen boundary.
//!
//! The migrator itself carries no crash hooks. The child process wraps its
//! real backend and calls `std::process::abort` when a statement of interest
//! has just finished (or is about to run), so the kill lands at an exact
//! boundary between executing DDL and recording progress.

use async_trait::async_trait;
use siderite_orm::{
    Backend, BackendCapabilities, ExecResult, Executor, IsolationLevel, OrmError, QueryPlan,
    QueryResult, Transaction, Value, WritePlan,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Where the wrapped process dies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Point {
    /// Immediately before the marked DDL script is sent.
    BeforeDdl,
    /// After the marked DDL script returned, before any progress write.
    AfterDdl,
    /// After a progress row insert/update affected a row.
    AfterProgress,
    /// After an intent row delete affected a row.
    AfterIntentFinish,
}

/// Crash configuration: a point, a statement marker and matches to skip.
pub struct Hook {
    point: Point,
    marker: String,
    skip: AtomicUsize,
    flag: std::path::PathBuf,
}

impl Hook {
    /// Parse `point:marker[:skip]` (the format written by [`Self::spec`]).
    pub fn parse(spec: &str, flag: std::path::PathBuf) -> Option<Self> {
        let mut parts = spec.splitn(3, ':');
        let point = match parts.next()? {
            "before-ddl" => Point::BeforeDdl,
            "after-ddl" => Point::AfterDdl,
            "after-progress" => Point::AfterProgress,
            "after-intent-finish" => Point::AfterIntentFinish,
            _ => return None,
        };
        let marker = parts.next()?.to_owned();
        let skip = parts.next().map_or(Some(0), |n| n.parse().ok())?;
        Some(Self {
            point,
            marker,
            skip: AtomicUsize::new(skip),
            flag,
        })
    }

    /// Encode a hook for the child environment.
    pub fn spec(point: &str, marker: &str, skip: usize) -> String {
        format!("{point}:{marker}:{skip}")
    }

    fn fire(&self) {
        let left = self.skip.load(Ordering::SeqCst);
        if left > 0 {
            self.skip.store(left - 1, Ordering::SeqCst);
            return;
        }
        // The flag proves the process died at the hook, not elsewhere.
        let _ = std::fs::write(&self.flag, "fired");
        // `abort` raises SIGABRT, so the host may write a core file.
        std::process::abort();
    }

    fn script(&self, sql: &str, done: bool) {
        let want = if done {
            Point::AfterDdl
        } else {
            Point::BeforeDdl
        };
        if self.point == want && sql.contains(&self.marker) {
            self.fire();
        }
    }

    fn raw(&self, sql: &str, affected: u64) {
        let upsert = sql.starts_with("INSERT INTO") || sql.starts_with("UPDATE");
        let hit = match self.point {
            Point::AfterProgress => upsert && sql.contains("siderite_migration_progress"),
            Point::AfterIntentFinish => {
                sql.starts_with("DELETE FROM") && sql.contains("siderite_migration_intents")
            }
            _ => false,
        };
        if hit && affected > 0 {
            self.fire();
        }
    }
}

/// Backend wrapper; every call is delegated to the real backend first.
pub struct CrashBackend {
    inner: Arc<dyn Backend>,
    hook: Arc<Hook>,
}

impl CrashBackend {
    /// Wrap `inner` with `hook`.
    pub fn new(inner: impl Backend, hook: Hook) -> Self {
        Self {
            inner: Arc::new(inner),
            hook: Arc::new(hook),
        }
    }
}

struct CrashTx {
    inner: Box<dyn Transaction>,
    hook: Arc<Hook>,
}

macro_rules! delegate {
    ($ty:ty) => {
        #[async_trait]
        impl Executor for $ty {
            fn capabilities(&self) -> BackendCapabilities {
                self.inner.capabilities()
            }

            async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
                self.inner.fetch(plan).await
            }

            async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
                self.inner.execute(plan).await
            }

            async fn fetch_raw(
                &self,
                sql: &str,
                params: Vec<Value>,
            ) -> Result<QueryResult, OrmError> {
                self.inner.fetch_raw(sql, params).await
            }

            async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
                let affected = self.inner.execute_raw(sql, params).await?;
                self.hook.raw(sql, affected);
                Ok(affected)
            }

            async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
                self.hook.script(sql, false);
                self.inner.execute_script(sql).await?;
                self.hook.script(sql, true);
                Ok(())
            }
        }
    };
}

delegate!(CrashBackend);
delegate!(CrashTx);

#[async_trait]
impl Backend for CrashBackend {
    fn read_parameter_count(&self, plan: &QueryPlan) -> Result<Option<usize>, OrmError> {
        self.inner.read_parameter_count(plan)
    }

    async fn begin(
        &self,
        isolation: Option<IsolationLevel>,
    ) -> Result<Box<dyn Transaction>, OrmError> {
        let inner = self.inner.begin(isolation).await?;
        Ok(Box::new(CrashTx {
            inner,
            hook: Arc::clone(&self.hook),
        }))
    }

    async fn begin_schema(&self, transactional: bool) -> Result<Box<dyn Transaction>, OrmError> {
        let inner = self.inner.begin_schema(transactional).await?;
        Ok(Box::new(CrashTx {
            inner,
            hook: Arc::clone(&self.hook),
        }))
    }
}

#[async_trait]
impl Transaction for CrashTx {
    async fn commit(&self) -> Result<(), OrmError> {
        self.inner.commit().await
    }

    async fn rollback(&self) -> Result<(), OrmError> {
        self.inner.rollback().await
    }
}
