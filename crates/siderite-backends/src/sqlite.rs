//! SQLite backend built on SQLx.
//!
//! Types without a native SQLite storage class are bound in their canonical
//! text form ([`canonical_text`]), so they compare and sort correctly and
//! decode through [`DbType::from_value`](siderite_orm::DbType::from_value).

use crate::shared::{TxSlot, affected_result, map_error, returning_result, with_tx};
use crate::sql::{CompiledQuery, Sqlite, compile, compile_write};
use async_trait::async_trait;
use siderite_orm::types::canonical_text;
use siderite_orm::{
    Backend, BackendCapabilities, BackendError, ExecResult, Executor, IsolationLevel, OrmError,
    QueryError, QueryPlan, QueryResult, Row, Transaction, Value, WritePlan,
};
use sqlx::pool::PoolConnection;
use sqlx::sqlite::{
    SqliteArguments, SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow,
    SqliteTransactionManager,
};
use sqlx::{Column as _, Row as _, TransactionManager, TypeInfo as _, ValueRef as _};
use std::collections::HashSet;
use std::time::Duration;

mod gate;
mod group;

use gate::{WriteGate, WritePermit};
use group::Committer;
pub use group::GroupCommit;

/// SQLite adapter executing compiled plans on a connection pool.
///
/// Writes from this backend and its clones (statements through
/// [`execute`](Executor::execute), [`execute_raw`](Executor::execute_raw)
/// and [`execute_script`](Executor::execute_script), transactions, and
/// transactional schema changes) queue for one write slot instead of racing
/// for SQLite's file lock, while reads keep using the whole pool. A write
/// sent through [`fetch_raw`](Executor::fetch_raw) bypasses the queue.
#[derive(Debug, Clone)]
pub struct SqliteBackend {
    pool: SqlitePool,
    gate: WriteGate,
    committer: Option<Committer>,
}

impl SqliteBackend {
    /// Connect to `url` (e.g. `sqlite::memory:` or `sqlite://app.db?mode=rwc`).
    ///
    /// SQLx enables foreign-key enforcement on every SQLite connection.
    ///
    /// # Errors
    /// Returns [`BackendError::Connection`] if the pool cannot be created.
    pub async fn connect(url: &str) -> Result<Self, BackendError> {
        // In-memory databases are per-connection, so a single connection keeps
        // state consistent for `sqlite::memory:`.
        let max = if url.contains(":memory:") { 1 } else { 10 };
        let pool = SqlitePoolOptions::new()
            .max_connections(max)
            .connect(url)
            .await
            .map_err(|e| BackendError::Connection(e.to_string()))?;
        Ok(Self::from_pool(pool))
    }

    /// Connect with explicit connection and pool options, for example to opt
    /// in to write-ahead logging:
    ///
    /// ```no_run
    /// # use siderite_backends::sqlite::SqliteBackend;
    /// # use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    /// # async fn open() -> Result<(), Box<dyn std::error::Error>> {
    /// let options: SqliteConnectOptions = "sqlite://app.db?mode=rwc".parse()?;
    /// let db = SqliteBackend::connect_with(
    ///     options.journal_mode(SqliteJournalMode::Wal),
    ///     SqlitePoolOptions::new().max_connections(10),
    /// )
    /// .await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [`connect`](Self::connect) leaves the journal mode and `synchronous`
    /// at the SQLite defaults (a rollback journal, synced on every commit).
    /// WAL is recorded in the database file and does not work on network
    /// filesystems; `synchronous = NORMAL` with WAL can lose the most recent
    /// commits on power loss. An in-memory database needs a pool of one
    /// connection.
    ///
    /// # Errors
    /// Returns [`BackendError::Connection`] if the pool cannot be created.
    pub async fn connect_with(
        options: SqliteConnectOptions,
        pool: SqlitePoolOptions,
    ) -> Result<Self, BackendError> {
        let pool = pool
            .connect_with(options)
            .await
            .map_err(|e| BackendError::Connection(e.to_string()))?;
        Ok(Self::from_pool(pool))
    }

    /// Wrap an existing pool.
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self {
            pool,
            gate: WriteGate::new(gate::DEFAULT_WAIT),
            committer: None,
        }
    }

    /// Opt in to group commit: concurrent [`execute`](Executor::execute)
    /// calls outside a transaction share one transaction and one commit
    /// sync, each isolated by a savepoint and acknowledged only after the
    /// commit. See [`GroupCommit`] for what changes compared with one
    /// transaction per write. Raw statements, scripts and transactions are
    /// unaffected.
    ///
    /// Call [`with_write_timeout`](Self::with_write_timeout) first; the
    /// committer keeps the timeout it started with.
    ///
    /// # Panics
    /// Outside a Tokio runtime, which runs the committer task.
    #[must_use]
    pub fn group_commit(mut self, config: GroupCommit) -> Self {
        let committer = Committer::spawn(self.pool.clone(), self.gate.clone(), config);
        self.committer = Some(committer);
        self
    }

    /// How long a write waits for the write slot before failing with
    /// "database is locked"; five seconds by default, like SQLx's busy
    /// timeout. Match it when configuring a different busy timeout.
    ///
    /// The slot is shared with existing clones; only this handle's wait
    /// changes.
    #[must_use]
    pub fn with_write_timeout(mut self, wait: Duration) -> Self {
        self.gate = self.gate.with_wait(wait);
        self
    }
}

#[async_trait]
impl Executor for SqliteBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::sqlite()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        let compiled = compile(plan, &Sqlite)?;
        fetch_rows(&self.pool, &compiled.sql, compiled.params).await
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        let compiled = compile_write(plan, &Sqlite)?;
        let returning = !plan.returning().is_empty();
        if let Some(committer) = &self.committer {
            return committer.submit(compiled, returning).await;
        }
        let _permit = self.gate.acquire().await?;
        run_write(&self.pool, compiled, returning).await
    }

    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        fetch_rows(&self.pool, sql, params).await
    }

    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        let _permit = self.gate.acquire().await?;
        execute_rows(&self.pool, sql, params).await
    }

    async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        let _permit = self.gate.acquire().await?;
        script_result(sqlx::Executor::execute(&self.pool, sql).await)
    }
}

#[async_trait]
impl Backend for SqliteBackend {
    fn read_parameter_count(&self, plan: &QueryPlan) -> Result<Option<usize>, OrmError> {
        Ok(Some(compile(plan, &Sqlite)?.params.len()))
    }

    async fn begin(
        &self,
        isolation: Option<IsolationLevel>,
    ) -> Result<Box<dyn Transaction>, OrmError> {
        if let Some(level) = isolation {
            self.capabilities()
                .require(siderite_orm::Feature::Isolation(level))?;
        }
        // Held for the whole transaction: it may write at any point.
        let permit = self.gate.acquire().await?;
        let tx = self.pool.begin().await.map_err(map_error)?;
        Ok(Box::new(SqliteTransaction {
            slot: TxSlot::new(tx),
            permit: Held::new(permit),
        }))
    }

    async fn begin_schema(&self, transactional: bool) -> Result<Box<dyn Transaction>, OrmError> {
        // `BEGIN IMMEDIATE` already excludes other writers. A non-transactional
        // schema connection stays ungated: callers keep writing through the
        // pool while it is open.
        let permit = if transactional {
            Some(self.gate.acquire().await?)
        } else {
            None
        };
        Ok(Box::new(
            SqliteSchemaTransaction::open(&self.pool, transactional, permit).await?,
        ))
    }
}

/// The write slot of an open transaction, released once it finishes.
///
/// Dropping a transaction without commit releases the slot before SQLx's
/// deferred rollback runs, so the next writer can briefly meet SQLite's busy
/// handler; it does not fail because of it.
struct Held(std::sync::Mutex<Option<WritePermit>>);

impl Held {
    fn new(permit: WritePermit) -> Self {
        Self(std::sync::Mutex::new(Some(permit)))
    }

    fn none() -> Self {
        Self(std::sync::Mutex::new(None))
    }

    fn release(&self) {
        drop(self.0.lock().map(|mut held| held.take()));
    }
}

/// An open SQLite transaction. Dropped without commit, it rolls back.
struct SqliteTransaction {
    slot: TxSlot<sqlx::Sqlite>,
    permit: Held,
}

#[async_trait]
impl Executor for SqliteTransaction {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::sqlite()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        let compiled = compile(plan, &Sqlite)?;
        with_tx!(self.slot, conn => fetch_rows(conn, &compiled.sql, compiled.params).await)
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        let compiled = compile_write(plan, &Sqlite)?;
        let returning = !plan.returning().is_empty();
        with_tx!(self.slot, conn => run_write(conn, compiled, returning).await)
    }

    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        with_tx!(self.slot, conn => fetch_rows(conn, sql, params).await)
    }

    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        with_tx!(self.slot, conn => execute_rows(conn, sql, params).await)
    }

    async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        with_tx!(self.slot, conn => script_result(sqlx::Executor::execute(conn, sql).await))
    }
}

#[async_trait]
impl Transaction for SqliteTransaction {
    async fn commit(&self) -> Result<(), OrmError> {
        let done = self.slot.commit().await;
        self.permit.release();
        done
    }

    async fn rollback(&self) -> Result<(), OrmError> {
        let done = self.slot.rollback().await;
        self.permit.release();
        done
    }
}

macro_rules! with_schema_conn {
    ($this:expr, $conn:ident => $body:expr) => {{
        let mut guard = $this.conn.lock().await;
        let conn = guard
            .as_mut()
            .ok_or(::siderite_orm::QueryError::TransactionClosed)?;
        let $conn = &mut **conn;
        $body
    }};
}

/// Dedicated connection with `PRAGMA foreign_keys` off around a schema change.
///
/// SQLite ignores `PRAGMA foreign_keys` inside a transaction, so the pragma
/// is applied *before* `BEGIN` and restored after commit or rollback.
///
/// The pre-commit `foreign_key_check` is scoped to violations this schema
/// change introduced: the rows at open are the baseline, and commit fails
/// only on rows that are not in it. Pre-existing violations in unrelated
/// tables must not block every future migration, and comparing rows (not
/// counts) means a change that fixes one violation while adding another
/// still fails. When foreign keys were already off (`restore_fk == 0`) the
/// check is skipped entirely.
struct SqliteSchemaTransaction {
    conn: tokio::sync::Mutex<Option<PoolConnection<sqlx::Sqlite>>>,
    restore_fk: i64,
    in_txn: bool,
    fk_baseline: Option<HashSet<FkViolation>>,
    permit: Held,
}

/// One `PRAGMA foreign_key_check` row: the offending child row and the parent
/// it points at. Identifies a violation independently of when it appeared.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FkViolation {
    table: String,
    rowid: i64,
    parent: String,
}

impl FkViolation {
    /// A missing or wrong-typed column becomes a placeholder, so an unreadable
    /// row still counts as a violation instead of being silently dropped.
    fn from_row(row: &SqliteRow) -> Self {
        let text = |key: &str| {
            row.try_get::<String, _>(key)
                .unwrap_or_else(|_| format!("<unreadable {key}>"))
        };
        let rowid = row.try_get("rowid").unwrap_or(-1);
        Self {
            table: text("table"),
            rowid,
            parent: text("parent"),
        }
    }

    fn location(&self) -> String {
        format!(
            "{} row {} (missing parent {})",
            self.table, self.rowid, self.parent
        )
    }
}

async fn fk_violations(
    conn: &mut PoolConnection<sqlx::Sqlite>,
) -> Result<Vec<SqliteRow>, OrmError> {
    Ok(sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut **conn)
        .await
        .map_err(map_error)?)
}

impl SqliteSchemaTransaction {
    async fn open(
        pool: &SqlitePool,
        transactional: bool,
        permit: Option<WritePermit>,
    ) -> Result<Self, OrmError> {
        let mut conn = pool.acquire().await.map_err(map_error)?;
        let restore_fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mut *conn)
            .await
            .map_err(map_error)?;
        // Baseline for the pre-commit check: only *new* violations fail the
        // migration. Skipped when FKs were already off (nothing to enforce).
        let fk_baseline: Option<HashSet<FkViolation>> = if restore_fk == 0 {
            None
        } else {
            Some(
                fk_violations(&mut conn)
                    .await?
                    .iter()
                    .map(FkViolation::from_row)
                    .collect(),
            )
        };
        set_foreign_keys(&mut conn, 0).await?;
        if transactional
            && let Err(err) =
                SqliteTransactionManager::begin(&mut *conn, Some("BEGIN IMMEDIATE".into())).await
        {
            drop(set_foreign_keys(&mut conn, restore_fk).await);
            return Err(map_error(err).into());
        }
        Ok(Self {
            conn: tokio::sync::Mutex::new(Some(conn)),
            restore_fk,
            in_txn: transactional,
            fk_baseline,
            permit: permit.map_or_else(Held::none, Held::new),
        })
    }

    async fn take(&self) -> Result<PoolConnection<sqlx::Sqlite>, OrmError> {
        self.conn
            .lock()
            .await
            .take()
            .ok_or_else(|| QueryError::TransactionClosed.into())
    }
}

impl Drop for SqliteSchemaTransaction {
    fn drop(&mut self) {
        if let Some(mut conn) = self.conn.get_mut().take() {
            if self.in_txn {
                SqliteTransactionManager::start_rollback(&mut *conn);
            }
            // Do not return a connection whose foreign_keys pragma we changed.
            conn.close_on_drop();
        }
    }
}

#[async_trait]
impl Executor for SqliteSchemaTransaction {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::sqlite()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        let compiled = compile(plan, &Sqlite)?;
        with_schema_conn!(self, conn => fetch_rows(conn, &compiled.sql, compiled.params).await)
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        let compiled = compile_write(plan, &Sqlite)?;
        let returning = !plan.returning().is_empty();
        with_schema_conn!(self, conn => run_write(conn, compiled, returning).await)
    }

    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        with_schema_conn!(self, conn => fetch_rows(conn, sql, params).await)
    }

    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        with_schema_conn!(self, conn => execute_rows(conn, sql, params).await)
    }

    async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        with_schema_conn!(self, conn => script_result(sqlx::Executor::execute(conn, sql).await))
    }
}

#[async_trait]
impl Transaction for SqliteSchemaTransaction {
    async fn commit(&self) -> Result<(), OrmError> {
        let done = self.finish_commit().await;
        self.permit.release();
        done
    }

    async fn rollback(&self) -> Result<(), OrmError> {
        let done = self.finish_rollback().await;
        self.permit.release();
        done
    }
}

impl SqliteSchemaTransaction {
    async fn finish_commit(&self) -> Result<(), OrmError> {
        let mut conn = self.take().await?;
        if let Some(baseline) = &self.fk_baseline {
            // Row-level diff, not a count: a change that repairs one old
            // violation while adding another keeps the count equal, and would
            // wrongly pass.
            let mut seen = HashSet::new();
            let mut fresh = Vec::new();
            for row in fk_violations(&mut conn).await? {
                let violation = FkViolation::from_row(&row);
                if !baseline.contains(&violation) && seen.insert(violation.clone()) {
                    fresh.push(violation);
                }
            }
            if !fresh.is_empty() {
                if self.in_txn {
                    drop(SqliteTransactionManager::rollback(&mut *conn).await);
                }
                drop(set_foreign_keys(&mut conn, self.restore_fk).await);
                // Without a transaction the statements are already
                // committed; say so rather than implying nothing happened.
                let outcome = if self.in_txn {
                    "rolled back"
                } else {
                    "already committed; fix the rows by hand"
                };
                return Err(BackendError::Constraint(format!(
                    "SQLite foreign_key_check found {} new violation(s) after schema change ({outcome}): {}",
                    fresh.len(),
                    violation_summary(&fresh),
                ))
                .into());
            }
        }
        if self.in_txn
            && let Err(err) = SqliteTransactionManager::commit(&mut *conn).await
        {
            drop(set_foreign_keys(&mut conn, self.restore_fk).await);
            return Err(map_error(err).into());
        }
        set_foreign_keys(&mut conn, self.restore_fk).await?;
        Ok(())
    }

    async fn finish_rollback(&self) -> Result<(), OrmError> {
        let mut conn = self.take().await?;
        let rollback = if self.in_txn {
            SqliteTransactionManager::rollback(&mut *conn)
                .await
                .map_err(map_error)
        } else {
            Ok(())
        };
        let restore = set_foreign_keys(&mut conn, self.restore_fk).await;
        rollback?;
        restore
    }
}

/// One-line summary of new `PRAGMA foreign_key_check` violations.
fn violation_summary(violations: &[FkViolation]) -> String {
    let mut locations: Vec<String> = violations.iter().map(FkViolation::location).collect();
    locations.sort();
    locations.truncate(5);
    if locations.is_empty() {
        "see PRAGMA foreign_key_check".to_owned()
    } else {
        locations.join(", ")
    }
}

async fn set_foreign_keys(
    conn: &mut PoolConnection<sqlx::Sqlite>,
    on: i64,
) -> Result<(), OrmError> {
    let sql = if on == 0 {
        "PRAGMA foreign_keys = OFF"
    } else {
        "PRAGMA foreign_keys = ON"
    };
    sqlx::query(sql)
        .execute(&mut **conn)
        .await
        .map(|_| ())
        .map_err(|e| map_error(e).into())
}

fn bind_all(
    sql: &str,
    params: Vec<Value>,
) -> sqlx::query::Query<'_, sqlx::Sqlite, SqliteArguments<'_>> {
    params.into_iter().fold(sqlx::query(sql), |query, param| {
        if let Some(text) = canonical_text(&param) {
            return query.bind(text);
        }
        match param {
            Value::Bool(b) => query.bind(b),
            Value::Int(i) => query.bind(i),
            Value::Float(f) => query.bind(f),
            Value::Text(s) => query.bind(s),
            Value::Bytes(b) => query.bind(b),
            // `canonical_text` covered every other variant.
            _ => query.bind(None::<i64>),
        }
    })
}

async fn fetch_rows<'c, E>(ex: E, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError>
where
    E: sqlx::Executor<'c, Database = sqlx::Sqlite>,
{
    let rows = bind_all(sql, params)
        .fetch_all(ex)
        .await
        .map_err(map_error)?;
    let rows = rows.iter().map(decode_row).collect::<Result<_, _>>()?;
    Ok(QueryResult { rows })
}

async fn execute_rows<'c, E>(ex: E, sql: &str, params: Vec<Value>) -> Result<u64, OrmError>
where
    E: sqlx::Executor<'c, Database = sqlx::Sqlite>,
{
    let done = bind_all(sql, params).execute(ex).await.map_err(map_error)?;
    Ok(done.rows_affected())
}

async fn run_write<'c, E>(
    ex: E,
    compiled: CompiledQuery,
    returning: bool,
) -> Result<ExecResult, OrmError>
where
    E: sqlx::Executor<'c, Database = sqlx::Sqlite>,
{
    if returning {
        let rows = fetch_rows(ex, &compiled.sql, compiled.params).await?.rows;
        Ok(returning_result(rows))
    } else {
        Ok(affected_result(
            execute_rows(ex, &compiled.sql, compiled.params).await?,
        ))
    }
}

fn script_result(
    done: Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error>,
) -> Result<(), OrmError> {
    done.map(|_| ()).map_err(|e| map_error(e).into())
}

fn decode_row(row: &SqliteRow) -> Result<Row, QueryError> {
    row.columns()
        .iter()
        .enumerate()
        .map(|(i, col)| {
            let name = col.name().to_owned();
            let decode_err = |e: sqlx::Error| QueryError::Decode {
                column: name.clone(),
                reason: e.to_string(),
            };
            let raw = row.try_get_raw(i).map_err(decode_err)?;
            let value = if raw.is_null() {
                Value::Null
            } else {
                match raw.type_info().name() {
                    "INTEGER" | "BOOLEAN" => Value::Int(row.try_get(i).map_err(decode_err)?),
                    "REAL" => Value::Float(row.try_get(i).map_err(decode_err)?),
                    "BLOB" => Value::Bytes(row.try_get(i).map_err(decode_err)?),
                    _ => Value::Text(row.try_get(i).map_err(decode_err)?),
                }
            };
            Ok((name, value))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Row::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use siderite_orm::expr::Field;
    use siderite_orm::{BackendCapabilityError, Expr, LockMode, OrderDirection};

    struct User;
    #[allow(non_upper_case_globals)]
    impl User {
        const name: Field<User, String> = Field::new("name");
        const age: Field<User, i64> = Field::new("age");
    }

    async fn seeded() -> SqliteBackend {
        let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
        Executor::execute_script(&db,
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER);
             INSERT INTO users (name, age) VALUES ('Alice', 30), ('bob', NULL), ('ALINA', 22), ('50%', 1);",
        )
        .await
        .unwrap();
        db
    }

    fn names(r: &QueryResult) -> Vec<String> {
        r.rows
            .iter()
            .map(|row| match row.get("name") {
                Some(Value::Text(s)) => s.clone(),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[tokio::test]
    async fn executes_filters_ordering_and_limits() {
        let db = seeded().await;
        let plan = QueryPlan::from_table("users")
            .filter(User::name.icontains("al"))
            .order_by(User::age, OrderDirection::Asc);
        assert_eq!(names(&db.fetch(&plan).await.unwrap()), ["ALINA", "Alice"]);

        let cs = QueryPlan::from_table("users").filter(User::name.contains("Al"));
        assert_eq!(names(&db.fetch(&cs).await.unwrap()), ["Alice"]);

        let nulls = QueryPlan::from_table("users").filter(User::age.is_null());
        assert_eq!(names(&db.fetch(&nulls).await.unwrap()), ["bob"]);

        let wildcard = QueryPlan::from_table("users").filter(User::name.icontains("%"));
        assert_eq!(names(&db.fetch(&wildcard).await.unwrap()), ["50%"]);

        let page = QueryPlan::from_table("users")
            .order_by(Expr::col("id"), OrderDirection::Asc)
            .offset(3);
        assert_eq!(names(&db.fetch(&page).await.unwrap()), ["50%"]);
    }

    #[tokio::test]
    async fn decodes_nulls_and_integers() {
        let db = seeded().await;
        let r = db
            .fetch(&QueryPlan::from_table("users").filter(User::name.eq("bob")))
            .await
            .unwrap();
        assert_eq!(r.rows[0].get("age"), Some(&Value::Null));
        assert_eq!(r.rows[0].get("id"), Some(&Value::Int(2)));
    }

    #[tokio::test]
    async fn row_locking_is_rejected_before_io() {
        let db = seeded().await;
        let err = db
            .fetch(&QueryPlan::from_table("users").lock(LockMode::ForUpdate))
            .await;
        assert!(matches!(
            err,
            Err(OrmError::Capability(
                BackendCapabilityError::RowLockingUnsupported { .. }
            ))
        ));
    }
}
