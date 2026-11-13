//! PostgreSQL backend built on SQLx.
//!
//! Every [`Value`] variant has a native PostgreSQL type (`Uuid` → `uuid`,
//! `Decimal` → `numeric`, `Timestamp` → `timestamptz`, `Json` → `jsonb`, ...),
//! so nothing is stored in a canonical text form. Rows are decoded by
//! PostgreSQL type name into the matching native variant.
//!
//! Connections are pinned to the `UTC` time zone so that `timestamptz`
//! date-part lookups (`EXTRACT`) agree with the values the ORM writes.

use crate::connection_init::ConnectionInit;
use crate::shared::{ConnSlot, TxSlot, with_conn, with_tx};
use crate::shared::{affected_result, decode_rows, map_error, returning_result};
use crate::sql::{CompiledQuery, Postgres, compile, compile_write};
use async_trait::async_trait;
use siderite_orm::{
    Backend, BackendCapabilities, BackendError, ExecResult, Executor, IsolationLevel, OrmError,
    QueryPlan, QueryResult, Transaction, Value, WritePlan,
};
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::postgres::types::Oid;
use sqlx::postgres::{
    PgArgumentBuffer, PgArguments, PgPool, PgPoolOptions, PgRow, PgTypeInfo, PgValueRef,
};
use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sqlx::types::uuid::Uuid;
use sqlx::types::{Decimal, JsonValue};
use sqlx::{Executor as _, Row as _, TypeInfo as _, ValueRef as _};

/// PostgreSQL adapter executing compiled plans on a connection pool.
#[derive(Debug, Clone)]
pub struct PgBackend {
    pool: PgPool,
}

impl PgBackend {
    /// Connect to `url` (`postgres://user:pass@host/db`) with a pool of up to
    /// ten connections, each set to the UTC time zone.
    ///
    /// A connection is **not** checked when it is taken from the pool: SQLx's
    /// `test_before_acquire` costs a round trip per query. The price is that
    /// the first query on a connection the server has closed (a restart, an
    /// idle timeout) fails with a connection error instead of being retried
    /// on a fresh connection. To have connections checked, use
    /// [`connect_with`](Self::connect_with) with
    /// `PgPoolOptions::new().test_before_acquire(true)`.
    ///
    /// # Errors
    /// [`BackendError::Connection`] if the pool cannot be created.
    pub async fn connect(url: &str) -> Result<Self, BackendError> {
        let options = PgPoolOptions::new()
            .max_connections(10)
            .test_before_acquire(false);
        Self::connect_with(url, options).await
    }

    /// Connect with pool settings and mandatory adapter initialization.
    ///
    /// Replaces any after_connect callback already stored in options.
    /// SQLx does not expose that callback for chaining; pass custom setup to
    /// [`Self::connect_with_init`] instead. Other pool options are retained.
    ///
    /// Args:
    ///     url: Database URL.
    ///     options: Pool capacity, timeouts and checkout policy.
    ///
    /// Returns:
    ///     Adapter whose fresh sessions receive mandatory initialization.
    ///
    /// # Errors
    /// Connection or initialization failure within the acquire deadline.
    pub async fn connect_with(url: &str, options: PgPoolOptions) -> Result<Self, BackendError> {
        let init: ConnectionInit<sqlx::Postgres> =
            std::sync::Arc::new(|_, _| Box::pin(async { Ok(()) }));
        Self::connect_with_init(url, options, init).await
    }

    /// Compose explicit custom setup with mandatory session settings.
    ///
    /// Args:
    ///     url: Database URL.
    ///     options: Pool settings; its stored after_connect is replaced.
    ///     init: Custom hook run first on every new/reconnected session.
    ///
    /// Returns:
    ///     Adapter after custom setup and required settings succeed.
    ///
    /// # Errors
    /// Connection or initialization failure within the acquire deadline.
    pub async fn connect_with_init(
        url: &str,
        options: PgPoolOptions,
        init: ConnectionInit<sqlx::Postgres>,
    ) -> Result<Self, BackendError> {
        let pool = options
            .after_connect(move |conn, meta| {
                let init = init.clone();
                Box::pin(async move {
                    init(conn, meta).await?;
                    conn.execute("SET TIME ZONE 'UTC'").await?;
                    Ok(())
                })
            })
            .connect(url)
            .await
            .map_err(|e| BackendError::Connection(e.to_string()))?;
        Ok(Self { pool })
    }

    /// Wrap an existing pool. Its connections should use the UTC time zone
    /// (see the module docs).
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl Executor for PgBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::postgres()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        let compiled = compile(plan, &Postgres)?;
        fetch_rows(&self.pool, &compiled.sql, compiled.params).await
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        let compiled = compile_write(plan, &Postgres)?;
        run_write(&self.pool, compiled, !plan.returning().is_empty()).await
    }

    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        fetch_rows(&self.pool, sql, params).await
    }

    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        execute_rows(&self.pool, sql, params).await
    }

    async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        script_result(sqlx::Executor::execute(&self.pool, sql).await)
    }
}

#[async_trait]
impl Backend for PgBackend {
    fn read_parameter_count(&self, plan: &QueryPlan) -> Result<Option<usize>, OrmError> {
        Ok(Some(compile(plan, &Postgres)?.params.len()))
    }

    async fn begin(
        &self,
        isolation: Option<IsolationLevel>,
    ) -> Result<Box<dyn Transaction>, OrmError> {
        if let Some(level) = isolation {
            self.capabilities()
                .require(siderite_orm::Feature::Isolation(level))?;
        }
        let mut tx = self.pool.begin().await.map_err(map_error)?;
        if let Some(level) = isolation {
            // Must be the first statement of the transaction.
            let statement = format!("SET TRANSACTION ISOLATION LEVEL {}", isolation_sql(level));
            sqlx::query(&statement)
                .execute(&mut *tx)
                .await
                .map_err(map_error)?;
        }
        Ok(Box::new(PgTransaction(TxSlot::new(tx))))
    }

    async fn begin_schema(&self, transactional: bool) -> Result<Box<dyn Transaction>, OrmError> {
        if transactional {
            return self.begin(None).await;
        }
        let conn = self.pool.acquire().await.map_err(map_error)?;
        Ok(Box::new(PgHeld(ConnSlot::new(conn))))
    }
}

fn isolation_sql(level: IsolationLevel) -> &'static str {
    match level {
        IsolationLevel::ReadCommitted => "READ COMMITTED",
        IsolationLevel::RepeatableRead => "REPEATABLE READ",
        IsolationLevel::Serializable => "SERIALIZABLE",
    }
}

/// An open PostgreSQL transaction. Dropped without commit, it rolls back.
struct PgTransaction(TxSlot<sqlx::Postgres>);

#[async_trait]
impl Executor for PgTransaction {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::postgres()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        let compiled = compile(plan, &Postgres)?;
        with_tx!(self.0, conn => fetch_rows(conn, &compiled.sql, compiled.params).await)
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        let compiled = compile_write(plan, &Postgres)?;
        let returning = !plan.returning().is_empty();
        with_tx!(self.0, conn => run_write(conn, compiled, returning).await)
    }

    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        with_tx!(self.0, conn => fetch_rows(conn, sql, params).await)
    }

    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        with_tx!(self.0, conn => execute_rows(conn, sql, params).await)
    }

    async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        with_tx!(self.0, conn => script_result(sqlx::Executor::execute(conn, sql).await))
    }
}

#[async_trait]
impl Transaction for PgTransaction {
    async fn commit(&self) -> Result<(), OrmError> {
        self.0.commit().await
    }

    async fn rollback(&self) -> Result<(), OrmError> {
        self.0.rollback().await
    }
}

/// Pooled connection held for a migration lock (no SQL transaction).
///
/// Commit and rollback close the connection instead of returning it to the
/// pool: a session lock (`pg_advisory_lock`) that was not released must never
/// be handed to another caller. Releasing the lock and then closing costs one
/// reconnect per migrate, which is negligible for a deploy-time operation.
struct PgHeld(ConnSlot<sqlx::Postgres>);

#[async_trait]
impl Executor for PgHeld {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::postgres()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        let compiled = compile(plan, &Postgres)?;
        with_conn!(self.0, conn => fetch_rows(conn, &compiled.sql, compiled.params).await)
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        let compiled = compile_write(plan, &Postgres)?;
        let returning = !plan.returning().is_empty();
        with_conn!(self.0, conn => run_write(conn, compiled, returning).await)
    }

    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        with_conn!(self.0, conn => fetch_rows(conn, sql, params).await)
    }

    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        with_conn!(self.0, conn => execute_rows(conn, sql, params).await)
    }

    async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        with_conn!(self.0, conn => script_result(sqlx::Executor::execute(conn, sql).await))
    }
}

#[async_trait]
impl Transaction for PgHeld {
    async fn commit(&self) -> Result<(), OrmError> {
        let mut conn = self.0.take().await?;
        conn.close_on_drop();
        drop(conn);
        Ok(())
    }

    async fn rollback(&self) -> Result<(), OrmError> {
        let mut conn = self.0.take().await?;
        conn.close_on_drop();
        drop(conn);
        Ok(())
    }
}

/// A `NULL` parameter of unspecified type (OID 0): the server infers it from
/// context, so it fits `text`, `uuid`, `timestamptz`, `jsonb`, ... columns.
/// (A typed `None::<i64>` would be rejected for all but integer columns.)
struct UntypedNull;

impl sqlx::Type<sqlx::Postgres> for UntypedNull {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(Oid(0))
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for UntypedNull {
    fn encode_by_ref(&self, _buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        Ok(IsNull::Yes)
    }
}

fn bind_all(sql: &str, params: Vec<Value>) -> sqlx::query::Query<'_, sqlx::Postgres, PgArguments> {
    params
        .into_iter()
        .fold(sqlx::query(sql), |query, param| match param {
            Value::Null => query.bind(UntypedNull),
            Value::Bool(v) => query.bind(v),
            Value::Int(v) => query.bind(v),
            Value::Float(v) => query.bind(v),
            Value::Decimal(v) => query.bind(v),
            Value::Text(v) => query.bind(v),
            Value::Bytes(v) => query.bind(v),
            Value::Uuid(v) => query.bind(v),
            Value::Date(v) => query.bind(v),
            Value::Time(v) => query.bind(v),
            Value::Timestamp(v) => query.bind(v),
            Value::Json(v) => query.bind(v),
        })
}

async fn fetch_rows<'c, E>(ex: E, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    let rows = bind_all(sql, params)
        .fetch_all(ex)
        .await
        .map_err(map_error)?;
    let rows = decode_rows(&rows, decode_value)?;
    Ok(QueryResult { rows })
}

async fn execute_rows<'c, E>(ex: E, sql: &str, params: Vec<Value>) -> Result<u64, OrmError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
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
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
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

fn script_result(done: Result<sqlx::postgres::PgQueryResult, sqlx::Error>) -> Result<(), OrmError> {
    done.map(|_| ()).map_err(|e| map_error(e).into())
}

/// Decode a non-null column by its PostgreSQL type name.
fn decode_value(row: &PgRow, i: usize, raw: PgValueRef<'_>) -> Result<Value, String> {
    /// `try_get` as `$t`, converted with `$into`.
    macro_rules! get {
        ($t:ty => $into:expr) => {
            row.try_get::<$t, _>(i)
                .map($into)
                .map_err(|e| e.to_string())
        };
    }
    /// A one-dimensional array as a JSON array.
    macro_rules! array {
        ($t:ty) => {
            get!(Vec<$t> => |items| Value::Json(JsonValue::Array(items.into_iter().map(JsonValue::from).collect())))
        };
    }
    let type_info = raw.type_info();
    match type_info.name() {
        "BOOL" => get!(bool => Value::Bool),
        "INT2" => get!(i16 => |v| Value::Int(i64::from(v))),
        "INT4" => get!(i32 => |v| Value::Int(i64::from(v))),
        "INT8" => get!(i64 => Value::Int),
        "FLOAT4" => get!(f32 => |v| Value::Float(f64::from(v))),
        "FLOAT8" => get!(f64 => Value::Float),
        "NUMERIC" => get!(Decimal => Value::Decimal),
        "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => get!(String => Value::Text),
        "BYTEA" => get!(Vec<u8> => Value::Bytes),
        "UUID" => get!(Uuid => Value::Uuid),
        "DATE" => get!(NaiveDate => Value::Date),
        "TIME" => get!(NaiveTime => Value::Time),
        "TIMESTAMPTZ" => get!(DateTime<Utc> => Value::Timestamp),
        "TIMESTAMP" => get!(NaiveDateTime => |v| Value::Timestamp(v.and_utc())),
        "JSON" | "JSONB" => get!(JsonValue => Value::Json),
        "INT2[]" => array!(i16),
        "INT4[]" => array!(i32),
        "INT8[]" => array!(i64),
        "FLOAT8[]" => array!(f64),
        "BOOL[]" => array!(bool),
        "TEXT[]" | "VARCHAR[]" => array!(String),
        other => Err(format!("unsupported PostgreSQL type {other}")),
    }
}
