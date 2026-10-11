//! Migration lock behaviour against real PostgreSQL and MySQL servers.
//!
//! Covers bounded waits, cancellation, server-side disconnects of the lock
//! holder and limited-privilege roles. Each case runs in a private scratch
//! database; limited roles/users are created inside the case and dropped
//! afterwards, even when an assertion panics.
//!
//! Run with `--include-ignored --test-threads=1`: the MySQL lock name is
//! server-wide, so cases must not overlap other migration tests.

mod common;

use async_trait::async_trait;
use common::scratch::ScratchDb;
use siderite_backends::{mysql::MySqlBackend, postgres::PgBackend};
use siderite_migrations::state::{FieldState, ModelState, SqlType};
use siderite_migrations::{Migration, MigrationError, MigrationGraph, Migrator};
use siderite_orm::{Backend, BackendCapabilities, Db};
use siderite_orm::{ExecResult, Executor, IsolationLevel};
use siderite_orm::{QueryPlan, QueryResult, Transaction, Value, WritePlan};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

type Failure = Box<dyn std::error::Error + Send + Sync>;
type Result = std::result::Result<(), Failure>;
type Res<T> = std::result::Result<T, Failure>;
type Mig<T> = std::result::Result<T, MigrationError>;
type Vals = Vec<Value>;
type Iso = Option<IsolationLevel>;
type Tx = Box<dyn Transaction>;
type Orm<T> = std::result::Result<T, siderite_orm::OrmError>;

/// PostgreSQL advisory key used by the migrator (`SIDE`).
const PG_KEY: i64 = 0x5349_4445;
const MYSQL_LOCK: &str = "siderite_migrate";
const SLEEP_MARK: &str = "lk_sleep_marker";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Engine {
    Pg,
    My,
}

/// Superuser-side inspection and control of the scratch database.
#[derive(Clone)]
struct Admin {
    engine: Engine,
    pg: Option<sqlx::PgPool>,
    my: Option<sqlx::MySqlPool>,
}

fn fail(e: impl std::fmt::Display) -> Failure {
    e.to_string().into()
}

impl Admin {
    async fn connect(engine: Engine, url: &str) -> Res<Self> {
        Ok(match engine {
            Engine::Pg => Self {
                engine,
                pg: Some(sqlx::PgPool::connect(url).await?),
                my: None,
            },
            Engine::My => Self {
                engine,
                pg: None,
                my: Some(sqlx::MySqlPool::connect(url).await?),
            },
        })
    }

    async fn close(&self) {
        if let Some(p) = &self.pg {
            p.close().await;
        }
        if let Some(p) = &self.my {
            p.close().await;
        }
    }

    async fn exec(&self, sql: &str) -> Result {
        match (&self.pg, &self.my) {
            (Some(p), _) => sqlx::query(sql).execute(p).await.map(drop),
            (_, Some(p)) => sqlx::query(sql).execute(p).await.map(drop),
            _ => Ok(()),
        }
        .map_err(fail)
    }

    async fn one(&self, pg: &str, my: &str) -> Res<Option<i64>> {
        let row = match self.engine {
            Engine::Pg => {
                let pool = self.pg.as_ref().ok_or("pg")?;
                sqlx::query_scalar::<_, Option<i64>>(pg)
                    .fetch_optional(pool)
                    .await?
            }
            Engine::My => {
                let pool = self.my.as_ref().ok_or("my")?;
                sqlx::query_scalar::<_, Option<i64>>(my)
                    .fetch_optional(pool)
                    .await?
            }
        };
        Ok(row.flatten())
    }

    /// Server session id that holds the migration lock, if any.
    async fn lock_holder(&self) -> Res<Option<i64>> {
        let pg = format!(
            "SELECT pid::bigint FROM pg_locks WHERE locktype = 'advisory' \
             AND granted AND classid = 0 AND objid = {PG_KEY} AND database = \
             (SELECT oid FROM pg_database WHERE datname = current_database()) \
             LIMIT 1"
        );
        let name = MYSQL_LOCK;
        let my = format!("SELECT CAST(IS_USED_LOCK('{name}') AS SIGNED)");
        self.one(&pg, &my).await
    }

    /// Session id currently running the marked sleeping statement.
    async fn sleeper(&self) -> Res<Option<i64>> {
        let pg = format!(
            "SELECT pid::bigint FROM pg_stat_activity WHERE datname = \
             current_database() AND query LIKE '%{SLEEP_MARK}%' AND pid <> \
             pg_backend_pid() AND state = 'active' LIMIT 1"
        );
        let my = format!(
            "SELECT CAST(id AS SIGNED) FROM information_schema.processlist \
             WHERE info LIKE '%{SLEEP_MARK}%' AND info NOT LIKE \
             '%processlist%' LIMIT 1"
        );
        self.one(&pg, &my).await
    }

    async fn kill(&self, session: i64) -> Result {
        match self.engine {
            Engine::Pg => {
                self.exec(&format!("SELECT pg_terminate_backend({session})"))
                    .await
            }
            Engine::My => self.exec(&format!("KILL {session}")).await,
        }
    }

    async fn tables(&self) -> Res<Vec<String>> {
        Ok(match (&self.pg, &self.my) {
            (Some(p), _) => {
                sqlx::query_scalar(
                    "SELECT tablename::text FROM pg_tables \
                     WHERE schemaname = 'public' ORDER BY 1",
                )
                .fetch_all(p)
                .await?
            }
            (_, Some(p)) => {
                sqlx::query_scalar(
                    "SELECT CAST(table_name AS CHAR) FROM \
                     information_schema.tables WHERE table_schema = \
                     DATABASE() ORDER BY 1",
                )
                .fetch_all(p)
                .await?
            }
            _ => Vec::new(),
        })
    }

    async fn wait_lock(&self, held: bool, within: Duration) -> Result {
        let start = Instant::now();
        loop {
            if self.lock_holder().await?.is_some() == held {
                return Ok(());
            }
            if start.elapsed() > within {
                let msg = format!("lock held != {held} after {within:?}");
                return Err(msg.into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_sleeper(&self) -> Res<i64> {
        let start = Instant::now();
        loop {
            if let Some(id) = self.sleeper().await? {
                return Ok(id);
            }
            if start.elapsed() > Duration::from_secs(15) {
                return Err("migration never reached its sleeping step".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// A plain session holding the migration lock, like another migrator would.
enum Holder {
    Pg(sqlx::PgConnection),
    My(sqlx::MySqlConnection),
}

impl Holder {
    async fn take(engine: Engine, url: &str) -> Res<Self> {
        use sqlx::Connection;
        Ok(match engine {
            Engine::Pg => {
                let mut c = sqlx::PgConnection::connect(url).await?;
                sqlx::query(&format!("SELECT pg_advisory_lock({PG_KEY})"))
                    .execute(&mut c)
                    .await?;
                Self::Pg(c)
            }
            Engine::My => {
                let mut c = sqlx::MySqlConnection::connect(url).await?;
                let sql = format!("SELECT GET_LOCK('{MYSQL_LOCK}', 0)");
                let q = sqlx::query_scalar::<_, Option<i64>>(&sql);
                let got = q.fetch_one(&mut c).await?;
                if got != Some(1) {
                    return Err("could not take the lock".into());
                }
                Self::My(c)
            }
        })
    }

    /// Release explicitly and report whether this session still owned it.
    async fn release(&mut self) -> Res<bool> {
        Ok(match self {
            Self::Pg(c) => {
                let sql = format!("SELECT pg_advisory_unlock({PG_KEY})");
                sqlx::query_scalar(&sql).fetch_one(c).await?
            }
            Self::My(c) => {
                let sql = format!("SELECT RELEASE_LOCK('{MYSQL_LOCK}')");
                let q = sqlx::query_scalar::<_, Option<i64>>(&sql);
                let r = q.fetch_one(c).await?;
                r == Some(1)
            }
        })
    }
}

struct Ctx {
    engine: Engine,
    db: Db,
    url: String,
    admin: Admin,
    roles: Arc<Mutex<Vec<String>>>,
}

fn with_credentials(url: &str, user: &str, pass: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let host = rest.rsplit_once('@').map_or(rest, |x| x.1);
    format!("{scheme}://{user}:{pass}@{host}")
}

async fn connect(engine: Engine, url: &str) -> Res<Db> {
    Ok(match engine {
        Engine::Pg => Db::new(PgBackend::connect(url).await?),
        Engine::My => Db::new(MySqlBackend::connect(url).await?),
    })
}

fn server_url(engine: Engine) -> Res<String> {
    let key = if engine == Engine::Pg {
        "DATABASE_URL"
    } else {
        "MYSQL_URL"
    };
    Ok(std::env::var(key).map_err(|_| format!("{key} is required"))?)
}

/// Run `case` on its own task so a panic still drops the scratch database
/// and every role created through [`Ctx::role`].
async fn run<F, Fut>(engine: Engine, case: F) -> Result
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = Result> + Send + 'static,
{
    let scratch = match engine {
        Engine::Pg => ScratchDb::postgres().await?,
        Engine::My => ScratchDb::mysql().await?,
    }
    .ok_or("no scratch database")?;
    let url = scratch.connection_url().to_owned();
    let admin = Admin::connect(engine, &url).await?;
    let roles = Arc::new(Mutex::new(Vec::new()));
    let ctx = Ctx {
        engine,
        db: scratch.db.clone(),
        url,
        admin: admin.clone(),
        roles: Arc::clone(&roles),
    };
    let joined = tokio::spawn(case(ctx)).await;
    admin.close().await;
    let cleanup = scratch.cleanup().await;
    let names = roles.lock().map(|r| r.clone()).unwrap_or_default();
    let server = server_url(engine)?;
    let dropped = drop_roles(engine, &server, &names).await;
    match joined {
        Ok(result) => {
            result?;
            cleanup?;
            dropped
        }
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => Err(e.into()),
    }
}

async fn drop_roles(engine: Engine, url: &str, names: &[String]) -> Result {
    if names.is_empty() {
        return Ok(());
    }
    let admin = Admin::connect(engine, url).await?;
    for name in names {
        match engine {
            Engine::Pg => {
                admin
                    .exec(&format!("DROP ROLE IF EXISTS \"{name}\""))
                    .await?
            }
            Engine::My => {
                admin
                    .exec(&format!("DROP USER IF EXISTS '{name}'@'%'"))
                    .await?
            }
        }
    }
    admin.close().await;
    Ok(())
}

impl Ctx {
    /// Create a login role/user and return its `Db` (named `grants` apply
    /// to the scratch database).
    async fn role(&self, grants: &[&str]) -> Res<Db> {
        let id = Uuid::new_v4().simple().to_string();
        let name = format!("lk_{}", &id[..12]);
        let dbname = self.url.rsplit_once('/').map_or("", |x| x.1).to_owned();
        if let Ok(mut r) = self.roles.lock() {
            r.push(name.clone());
        }
        let q = format!("\"{name}\"");
        let setup: Vec<String> = match self.engine {
            Engine::Pg => vec![
                format!("CREATE ROLE {q} LOGIN PASSWORD 'pw'"),
                format!("GRANT CONNECT ON DATABASE \"{dbname}\" TO {q}"),
                "REVOKE CREATE ON SCHEMA public FROM PUBLIC".to_owned(),
                format!("GRANT USAGE ON SCHEMA public TO {q}"),
            ],
            Engine::My => {
                vec![format!("CREATE USER '{name}'@'%' IDENTIFIED BY 'pw'")]
            }
        };
        for sql in setup {
            self.admin.exec(&sql).await?;
        }
        for grant in grants {
            let sql = match self.engine {
                Engine::Pg => grant.replace("{role}", &format!("\"{name}\"")),
                Engine::My => grant
                    .replace("{role}", &format!("'{name}'@'%'"))
                    .replace("{db}", &format!("`{dbname}`")),
            };
            self.admin.exec(&sql).await?;
        }
        connect(self.engine, &with_credentials(&self.url, &name, "pw")).await
    }
}

fn model(table: &str) -> Operation {
    let id = FieldState {
        primary_key: true,
        ..FieldState::new("id", "id", SqlType::BigInt)
    };
    Operation::CreateModel {
        model: ModelState {
            name: table.into(),
            table: table.into(),
            fields: vec![id],
            indexes: Vec::new(),
            constraints: Vec::new(),
        },
    }
}

use siderite_migrations::Operation;

fn graph(migrations: Vec<Migration>) -> Mig<MigrationGraph> {
    MigrationGraph::build(migrations)
}

fn quick(id: &str, deps: &[&str], table: &str) -> Mig<Migration> {
    let deps = deps.iter().map(|d| (*d).to_owned()).collect();
    Migration::new(id, deps, vec![model(table)], true, vec![])
}

/// A migration whose only step sleeps for `secs` seconds.
fn slow(engine: Engine, id: &str, secs: u32) -> Mig<Migration> {
    let sql = match engine {
        Engine::Pg => format!("SELECT pg_sleep({secs}) /* {SLEEP_MARK} */"),
        Engine::My => format!("SELECT SLEEP({secs}) /* {SLEEP_MARK} */"),
    };
    let op = Operation::RunSQL {
        sql,
        reverse_sql: None,
    };
    Migration::new(id, vec![], vec![op], true, vec![])
}

type Rep = siderite_migrations::Report;

async fn go(db: &Db, g: &MigrationGraph, ms: u64) -> Mig<Rep> {
    Migrator::new(db, g)
        .with_lock_timeout(Duration::from_millis(ms))?
        .migrate(None, false)
        .await
}

async fn applied(db: &Db, g: &MigrationGraph) -> Res<Vec<String>> {
    Ok(Migrator::new(db, g).inspect_recovery().await?.applied)
}

// ---------------------------------------------------------------- cases

/// A second migrator gives up within its bound while the lock is held, the
/// holder keeps ownership, and a later run succeeds once it is released.
async fn bounded_wait(ctx: Ctx) -> Result {
    let g = graph(vec![quick("0001_a", &[], "lk_a")?])?;
    let mut holder = Holder::take(ctx.engine, &ctx.url).await?;
    let start = Instant::now();
    let err = go(&ctx.db, &g, 600)
        .await
        .err()
        .ok_or("migrate succeeded while the lock was held")?;
    let waited = start.elapsed();
    assert!(matches!(err, MigrationError::LockTimeout), "{err:?}");
    assert_eq!(err.to_string(), "timed out waiting for the migration lock");
    assert!(
        waited >= Duration::from_millis(550),
        "gave up early: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(5),
        "unbounded wait: {waited:?}"
    );
    // Nothing was written, and the waiter did not steal or drop the lock.
    assert!(ctx.admin.tables().await?.is_empty());
    assert!(holder.release().await?, "holder lost ownership");
    ctx.admin.wait_lock(false, Duration::from_secs(5)).await?;
    go(&ctx.db, &g, 5_000).await?;
    assert_eq!(applied(&ctx.db, &g).await?, ["0001_a"]);
    ctx.admin.wait_lock(false, Duration::from_secs(5)).await
}

/// Aborting a waiting migrator leaves no lock or pending request behind.
async fn cancel_waiter(ctx: Ctx) -> Result {
    let g = graph(vec![quick("0001_a", &[], "lk_a")?])?;
    let mut holder = Holder::take(ctx.engine, &ctx.url).await?;
    let waiter = {
        let (db, g) = (ctx.db.clone(), g.clone());
        tokio::spawn(async move {
            Migrator::new(&db, &g)
                .with_lock_timeout(Duration::from_secs(60))?
                .migrate(None, false)
                .await
                .map(drop)
                .map_err(fail)
        })
    };
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!waiter.is_finished(), "waiter should still be waiting");
    waiter.abort();
    assert!(waiter.await.err().is_some_and(|e| e.is_cancelled()));
    assert!(holder.release().await?);
    // The cancelled waiter must not grab the lock after the release.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(ctx.admin.lock_holder().await?.is_none(), "leaked lock");
    assert!(ctx.admin.tables().await?.is_empty());
    go(&ctx.db, &g, 5_000).await?;
    Ok(())
}

/// Dropping a migrator that is mid-run closes its session, which releases
/// the lock; the next migrator proceeds and nothing is half recorded.
async fn cancel_holder(ctx: Ctx) -> Result {
    let g = graph(vec![slow(ctx.engine, "0001_slow", 6)?])?;
    let task = {
        let (db, g) = (ctx.db.clone(), g.clone());
        tokio::spawn(async move {
            Migrator::new(&db, &g)
                .migrate(None, false)
                .await
                .map(drop)
                .map_err(fail)
        })
    };
    ctx.admin.wait_sleeper().await?;
    assert!(ctx.admin.lock_holder().await?.is_some());
    task.abort();
    assert!(task.await.err().is_some_and(|e| e.is_cancelled()));
    // The server finishes the in-flight statement before it sees the
    // closed connection, so release is bounded by that statement (6s).
    ctx.admin.wait_lock(false, Duration::from_secs(20)).await?;
    let g2 = graph(vec![quick("0002_b", &[], "lk_b")?])?;
    go(&ctx.db, &g2, 5_000).await?;
    assert_eq!(applied(&ctx.db, &g2).await?, ["0002_b"]);
    assert!(
        !applied(&ctx.db, &g)
            .await?
            .contains(&"0001_slow".to_owned())
    );
    Ok(())
}

/// The holder's session is killed server-side mid-run.
async fn kill_holder(ctx: Ctx) -> Result {
    let g = graph(vec![slow(ctx.engine, "0001_slow", 30)?])?;
    let task = {
        let (db, g) = (ctx.db.clone(), g.clone());
        tokio::spawn(async move { go(&db, &g, 30_000).await })
    };
    let victim = ctx.admin.wait_sleeper().await?;
    assert_eq!(ctx.admin.lock_holder().await?, Some(victim), "same session");
    ctx.admin.kill(victim).await?;
    let outcome = tokio::time::timeout(Duration::from_secs(15), task).await??;
    let err = outcome.err().ok_or("killed run reported success")?;
    // The run did not complete: a driver/connection error, never success,
    // and no migration is recorded.
    assert!(
        matches!(err, MigrationError::Orm(_) | MigrationError::State(_)),
        "{err:?}"
    );
    ctx.admin.wait_lock(false, Duration::from_secs(10)).await?;
    assert!(applied(&ctx.db, &g).await?.is_empty());
    let g2 = graph(vec![quick("0002_b", &[], "lk_b")?])?;
    go(&ctx.db, &g2, 5_000).await?;
    assert_eq!(applied(&ctx.db, &g2).await?, ["0002_b"]);
    Ok(())
}

/// Kills the lock session right before the release statement, so the work
/// is done and recorded but ownership cannot be confirmed.
struct KillBeforeRelease {
    inner: Arc<dyn Backend>,
    admin: Admin,
    armed: Arc<AtomicBool>,
}

struct KillTx {
    inner: Box<dyn Transaction>,
    admin: Admin,
    armed: Arc<AtomicBool>,
}

async fn maybe_kill(admin: &Admin, armed: &AtomicBool, sql: &str) -> Orm<()> {
    let release = ["pg_advisory_unlock", "RELEASE_LOCK"]
        .iter()
        .any(|k| sql.contains(k));
    if release && armed.swap(false, Ordering::SeqCst) {
        let holder = admin.lock_holder().await.ok().flatten();
        if let Some(id) = holder {
            let _ = admin.kill(id).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
    Ok(())
}

macro_rules! delegate {
    ($ty:ty) => {
        #[async_trait]
        impl Executor for $ty {
            fn capabilities(&self) -> BackendCapabilities {
                self.inner.capabilities()
            }
            async fn fetch(&self, plan: &QueryPlan) -> Orm<QueryResult> {
                self.inner.fetch(plan).await
            }
            async fn execute(&self, plan: &WritePlan) -> Orm<ExecResult> {
                self.inner.execute(plan).await
            }
            async fn fetch_raw(&self, q: &str, p: Vals) -> Orm<QueryResult> {
                maybe_kill(&self.admin, &self.armed, q).await?;
                self.inner.fetch_raw(q, p).await
            }
            async fn execute_raw(&self, sql: &str, params: Vals) -> Orm<u64> {
                self.inner.execute_raw(sql, params).await
            }
            async fn execute_script(&self, sql: &str) -> Orm<()> {
                self.inner.execute_script(sql).await
            }
        }
    };
}

delegate!(KillBeforeRelease);
delegate!(KillTx);

#[async_trait]
impl Backend for KillBeforeRelease {
    fn read_parameter_count(&self, plan: &QueryPlan) -> Orm<Option<usize>> {
        self.inner.read_parameter_count(plan)
    }

    async fn begin(&self, isolation: Iso) -> Orm<Tx> {
        self.inner.begin(isolation).await
    }

    async fn begin_schema(&self, transactional: bool) -> Orm<Tx> {
        let inner = self.inner.begin_schema(transactional).await?;
        Ok(Box::new(KillTx {
            inner,
            admin: self.admin.clone(),
            armed: Arc::clone(&self.armed),
        }))
    }
}

#[async_trait]
impl Transaction for KillTx {
    async fn commit(&self) -> Orm<()> {
        self.inner.commit().await
    }

    async fn rollback(&self) -> Orm<()> {
        self.inner.rollback().await
    }
}

async fn kill_before_release(ctx: Ctx) -> Result {
    let inner: Arc<dyn Backend> = match ctx.engine {
        Engine::Pg => Arc::new(PgBackend::connect(&ctx.url).await?),
        Engine::My => Arc::new(MySqlBackend::connect(&ctx.url).await?),
    };
    let armed = Arc::new(AtomicBool::new(true));
    let db = Db::new(KillBeforeRelease {
        inner,
        admin: ctx.admin.clone(),
        armed: Arc::clone(&armed),
    });
    let g = graph(vec![quick("0001_a", &[], "lk_a")?])?;
    let err = Migrator::new(&db, &g)
        .migrate(None, false)
        .await
        .err()
        .ok_or("lost release was reported as plain success")?;
    assert!(!armed.load(Ordering::SeqCst), "kill never fired");
    match &err {
        MigrationError::LockReleaseUncertain { applied, .. } => {
            assert_eq!(applied, &["0001_a"]);
        }
        other => {
            return Err(format!("not reported explicitly: {other:?}").into());
        }
    }
    let text = err.to_string();
    assert!(text.contains("lock ownership is uncertain"), "{text}");
    // The work really did commit, and the server dropped the lock.
    ctx.admin.wait_lock(false, Duration::from_secs(10)).await?;
    assert_eq!(applied(&ctx.db, &g).await?, ["0001_a"]);
    // A re-run is a clean no-op rather than a double apply.
    go(&ctx.db, &g, 5_000).await?;
    Ok(())
}

fn denied(text: &str) -> bool {
    let t = text.to_lowercase();
    ["permission denied", "command denied", "access denied"]
        .iter()
        .any(|k| t.contains(k))
}

/// A role without CREATE: clear permission error, nothing created, lock
/// free afterwards.
async fn no_create(ctx: Ctx) -> Result {
    let grants: &[&str] = match ctx.engine {
        Engine::Pg => &[],
        Engine::My => &["GRANT SELECT ON {db}.* TO {role}"],
    };
    let limited = ctx.role(grants).await?;
    let g = graph(vec![quick("0001_a", &[], "lk_a")?])?;
    let err = Migrator::new(&limited, &g)
        .with_lock_timeout(Duration::from_secs(5))?
        .migrate(None, false)
        .await
        .err()
        .ok_or("limited role migrated")?;
    let text = format!("{err} / {err:?}");
    assert!(denied(&text), "unclear diagnostic: {text}");
    ctx.admin.wait_lock(false, Duration::from_secs(10)).await?;
    assert!(ctx.admin.tables().await?.is_empty(), "partial state");
    // The same graph then runs fine with full privileges.
    go(&ctx.db, &g, 5_000).await?;
    assert_eq!(applied(&ctx.db, &g).await?, ["0001_a"]);
    Ok(())
}

/// A role that may CREATE but cannot write the bookkeeping tables: the
/// failure is a clear permission error, history is untouched and the lock
/// is released. Table-level side effects are engine specific (MySQL DDL
/// commits implicitly; PostgreSQL rolls the atomic migration back).
async fn bookkeeping_read_only(ctx: Ctx) -> Result {
    let first = graph(vec![quick("0001_a", &[], "lk_a")?])?;
    go(&ctx.db, &first, 5_000).await?;
    let grants: &[&str] = match ctx.engine {
        Engine::Pg => &[
            "GRANT CREATE ON SCHEMA public TO {role}",
            "GRANT SELECT ON ALL TABLES IN SCHEMA public TO {role}",
        ],
        Engine::My => &["GRANT SELECT, CREATE ON {db}.* TO {role}"],
    };
    let limited = ctx.role(grants).await?;
    let both = graph(vec![
        quick("0001_a", &[], "lk_a")?,
        quick("0002_b", &["0001_a"], "lk_b")?,
    ])?;
    let before = ctx.admin.tables().await?;
    let err = Migrator::new(&limited, &both)
        .with_lock_timeout(Duration::from_secs(5))?
        .migrate(None, false)
        .await
        .err()
        .ok_or("read-only bookkeeping role migrated")?;
    let text = format!("{err} / {err:?}");
    assert!(denied(&text), "unclear diagnostic: {text}");
    ctx.admin.wait_lock(false, Duration::from_secs(10)).await?;
    assert_eq!(applied(&ctx.db, &both).await?, ["0001_a"]);
    let after = ctx.admin.tables().await?;
    match ctx.engine {
        Engine::Pg => assert_eq!(after, before, "PostgreSQL left partial DDL"),
        Engine::My => {
            let new = |t: &&String| !before.contains(t);
            let extra: Vec<_> = after.iter().filter(new).collect();
            assert!(extra.iter().all(|t| *t == "lk_b"), "{extra:?}");
        }
    }
    Ok(())
}

macro_rules! live {
    ($name:ident, $case:ident, $engine:expr) => {
        #[tokio::test]
        #[ignore = "live PostgreSQL/MySQL; use --include-ignored"]
        async fn $name() -> Result {
            run($engine, $case).await
        }
    };
}

live!(bounded_wait_postgres, bounded_wait, Engine::Pg);
live!(bounded_wait_mysql, bounded_wait, Engine::My);
live!(cancel_waiter_postgres, cancel_waiter, Engine::Pg);
live!(cancel_waiter_mysql, cancel_waiter, Engine::My);
live!(cancel_holder_postgres, cancel_holder, Engine::Pg);
live!(cancel_holder_mysql, cancel_holder, Engine::My);
live!(kill_holder_postgres, kill_holder, Engine::Pg);
live!(kill_holder_mysql, kill_holder, Engine::My);
live!(
    kill_before_release_postgres,
    kill_before_release,
    Engine::Pg
);
live!(kill_before_release_mysql, kill_before_release, Engine::My);
live!(no_create_postgres, no_create, Engine::Pg);
live!(no_create_mysql, no_create, Engine::My);
live!(
    bookkeeping_read_only_postgres,
    bookkeeping_read_only,
    Engine::Pg
);
live!(
    bookkeeping_read_only_mysql,
    bookkeeping_read_only,
    Engine::My
);
