//! Test-only backend wrapper that kills the process at a chosen boundary.
//!
//! The migrator itself carries no crash hooks. The child process wraps its
//! real backend and calls `std::process::abort` when a statement of interest
//! has just finished (or is about to run), so the kill lands at an exact
//! boundary between executing DDL and recording progress.

use async_trait::async_trait;
use siderite_orm::{Backend, BackendCapabilities, BackendKind, Db};
use siderite_orm::{ExecResult, Executor, IsolationLevel, OrmError};
use siderite_orm::{QueryPlan, QueryResult, Transaction, Value, WritePlan};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

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
    /// Immediately before the history row insert is sent.
    BeforeHistoryInsert,
    /// After the history row insert affected a row.
    AfterHistoryInsert,
    /// Immediately before the history row delete is sent.
    BeforeHistoryDelete,
    /// After the history row delete affected a row.
    AfterHistoryDelete,
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
            "before-history-insert" => Point::BeforeHistoryInsert,
            "after-history-insert" => Point::AfterHistoryInsert,
            "before-history-delete" => Point::BeforeHistoryDelete,
            "after-history-delete" => Point::AfterHistoryDelete,
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

    /// The history table name never occurs inside the progress or intent
    /// table names, so a substring test identifies history writes.
    fn history(sql: &str, verb: &str) -> bool {
        sql.starts_with(verb) && sql.contains("siderite_migrations")
    }

    fn before_raw(&self, sql: &str) {
        let hit = match self.point {
            Point::BeforeHistoryInsert => Self::history(sql, "INSERT INTO"),
            Point::BeforeHistoryDelete => Self::history(sql, "DELETE FROM"),
            _ => false,
        };
        if hit {
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
            Point::AfterHistoryInsert => Self::history(sql, "INSERT INTO"),
            Point::AfterHistoryDelete => Self::history(sql, "DELETE FROM"),
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
                self.hook.before_raw(sql);
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

type Failure = Box<dyn std::error::Error + Send + Sync>;

/// Kills and reaps the child if the parent gives up early.
struct OwnedChild(std::process::Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Run the ignored test `child` of the current test binary until it dies
/// at its hook, and prove the hook fired.
///
/// `flag_var` names the environment variable through which the child
/// learns the flag path its hook writes; `envs` configures the rest. The
/// child's stderr goes to a file whose tail is part of the error, so a
/// child that dies elsewhere (a panic, a connect error) explains itself.
/// The hook calls `abort()`, which raises SIGABRT, so the child may leave
/// a core file behind depending on the host's core pattern.
pub async fn run_child(
    child: &str,
    flag_var: &str,
    envs: &[(&str, String)],
) -> Result<(), Failure> {
    let stem = format!("siderite-crash-{}", uuid::Uuid::new_v4().simple());
    let flag = std::env::temp_dir().join(&stem);
    let log = std::env::temp_dir().join(format!("{stem}.stderr"));
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args(["--ignored", "--exact", child])
        .env(flag_var, &flag)
        .envs(envs.iter().map(|(k, v)| (*k, v.as_str())))
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(&log)?);
    let mut owned = OwnedChild(command.spawn()?);
    let waited = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Some(status) = owned.0.try_wait()? {
                return Ok::<_, std::io::Error>(status);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let fired = flag.exists();
    let stderr = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&flag);
    let _ = std::fs::remove_file(&log);
    let status = match waited {
        Ok(status) => status?,
        Err(_) => return Err(child_error("timed out after 60s", &stderr)),
    };
    if fired && !status.success() {
        return Ok(());
    }
    Err(child_error(&status.to_string(), &stderr))
}

fn child_error(status: &str, stderr: &str) -> Failure {
    let lines: Vec<&str> = stderr.lines().collect();
    let tail = lines[lines.len().saturating_sub(40)..].join("\n");
    format!("child did not die at its hook ({status}):\n{tail}").into()
}

/// Whether `table` exists, read from the catalog of the current database
/// or schema (not inferred from a failing `SELECT`).
pub async fn table_exists(db: &Db, table: &str) -> Result<bool, Failure> {
    let sql = match db.capabilities().kind {
        BackendKind::Postgres => {
            "SELECT COUNT(*) AS n FROM pg_catalog.pg_tables \
             WHERE schemaname = current_schema() AND tablename = $1"
        }
        BackendKind::MySql => {
            "SELECT COUNT(*) AS n FROM information_schema.tables \
             WHERE table_schema = DATABASE() AND table_name = ?"
        }
        other => return Err(format!("no catalog query for {other:?}").into()),
    };
    let rows = db.raw_sql(sql, vec![Value::from(table)]).await?.rows;
    match rows.first().and_then(|row| row.get("n")) {
        Some(Value::Int(n)) => Ok(*n > 0),
        other => Err(format!("unexpected catalog count {other:?}").into()),
    }
}
