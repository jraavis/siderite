//! SQLx-specific MySQL binding, execution and decoding.

use super::db_error;
use super::io::{MySqlIo, WriteDone};
use crate::shared::decode_rows;
use siderite_orm::{OrmError, QueryResult, Value};
use sqlx::mysql::{MySqlArguments, MySqlRow, MySqlValueRef};
use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sqlx::types::{Decimal, JsonValue};
use sqlx::{Row as _, TypeInfo as _, ValueRef as _};

type Bound<'q> = sqlx::query::Query<'q, sqlx::MySql, MySqlArguments>;

fn bind_all(sql: &str, params: Vec<Value>) -> Bound<'_> {
    params
        .into_iter()
        .fold(sqlx::query(sql), |query, param| match param {
            Value::Null => query.bind(None::<i64>),
            Value::Bool(v) => query.bind(v),
            Value::Int(v) => query.bind(v),
            Value::Float(v) => query.bind(v),
            Value::Decimal(v) => query.bind(v),
            Value::Text(v) => query.bind(v),
            Value::Bytes(v) => query.bind(v),
            // SQLx would send 16 raw bytes; the canonical storage is CHAR(36).
            Value::Uuid(v) => query.bind(v.hyphenated().to_string()),
            Value::Date(v) => query.bind(v),
            Value::Time(v) => query.bind(v),
            Value::Timestamp(v) => query.bind(v),
            Value::Json(v) => query.bind(v),
        })
}

pub(super) async fn fetch_rows<'c, E>(
    executor: E,
    sql: &str,
    params: Vec<Value>,
) -> Result<QueryResult, OrmError>
where
    E: sqlx::Executor<'c, Database = sqlx::MySql>,
{
    let rows = bind_all(sql, params)
        .fetch_all(executor)
        .await
        .map_err(db_error)?;
    let rows = decode_rows(&rows, value)?;
    Ok(QueryResult { rows })
}

pub(super) async fn execute_rows<'c, E>(
    executor: E,
    sql: &str,
    params: Vec<Value>,
) -> Result<u64, OrmError>
where
    E: sqlx::Executor<'c, Database = sqlx::MySql>,
{
    Ok(run(executor, sql, params).await?.rows_affected())
}

pub(super) async fn run<'c, E>(
    executor: E,
    sql: &str,
    parameters: Vec<Value>,
) -> Result<WriteDone, OrmError>
where
    E: sqlx::Executor<'c, Database = sqlx::MySql>,
{
    let done = bind_all(sql, parameters)
        .execute(executor)
        .await
        .map_err(db_error)?;
    Ok(WriteDone {
        affected: done.rows_affected(),
        key: done.last_insert_id(),
    })
}

// Decode wire values with the same canonical storage mapping as binding.

type Wire<'a> = MySqlValueRef<'a>;

/// Decode a non-null column by its MySQL type name.
fn value(row: &MySqlRow, i: usize, raw: Wire<'_>) -> Result<Value, String> {
    /// `try_get` as `$t`, converted with `$into`.
    macro_rules! get {
        ($t:ty => $into:expr) => {
            row.try_get::<$t, _>(i)
                .map($into)
                .map_err(|e| e.to_string())
        };
    }
    let type_info = raw.type_info();
    match type_info.name() {
        "BOOLEAN" => get!(bool => Value::Bool),
        "TINYINT" => get!(i8 => |v| Value::Int(i64::from(v))),
        "SMALLINT" => get!(i16 => |v| Value::Int(i64::from(v))),
        "INT" | "MEDIUMINT" => get!(i32 => |v| Value::Int(i64::from(v))),
        "BIGINT" => get!(i64 => Value::Int),
        "TINYINT UNSIGNED" => get!(u8 => |v| Value::Int(i64::from(v))),
        "SMALLINT UNSIGNED" | "YEAR" => get!(u16 => integer),
        "INT UNSIGNED" | "MEDIUMINT UNSIGNED" => get!(u32 => integer),
        "BIGINT UNSIGNED" => row
            .try_get::<u64, _>(i)
            .map_err(|e| e.to_string())
            .and_then(|v| {
                i64::try_from(v)
                    .map(Value::Int)
                    .map_err(|_| integer_overflow(v))
            }),
        "FLOAT" => get!(f32 => |v| Value::Float(f64::from(v))),
        "DOUBLE" => get!(f64 => Value::Float),
        "DECIMAL" => get!(Decimal => Value::Decimal),
        "CHAR" | "VARCHAR" | "ENUM" | "SET" => get!(String => Value::Text),
        "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" => {
            get!(String => Value::Text)
        }
        "BINARY" | "VARBINARY" => get!(Vec<u8> => Value::Bytes),
        "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" => {
            get!(Vec<u8> => Value::Bytes)
        }
        "DATE" => get!(NaiveDate => Value::Date),
        "TIME" => get!(NaiveTime => Value::Time),
        "DATETIME" => get!(NaiveDateTime => |v| Value::Timestamp(v.and_utc())),
        "TIMESTAMP" => get!(DateTime<Utc> => Value::Timestamp),
        "JSON" => get!(JsonValue => Value::Json),
        other => Err(format!("unsupported MySQL type {other}")),
    }
}

impl MySqlIo for sqlx::MySqlConnection {
    async fn fetch(
        &mut self,
        statement: &str,
        parameters: Vec<Value>,
    ) -> Result<QueryResult, OrmError> {
        fetch_rows(&mut *self, statement, parameters).await
    }

    async fn run(
        &mut self,
        statement: &str,
        parameters: Vec<Value>,
    ) -> Result<WriteDone, OrmError> {
        run(&mut *self, statement, parameters).await
    }
}

impl MySqlIo for &sqlx::MySqlPool {
    async fn fetch(
        &mut self,
        statement: &str,
        parameters: Vec<Value>,
    ) -> Result<QueryResult, OrmError> {
        fetch_rows(*self, statement, parameters).await
    }

    async fn run(
        &mut self,
        statement: &str,
        parameters: Vec<Value>,
    ) -> Result<WriteDone, OrmError> {
        run(*self, statement, parameters).await
    }
}

fn integer<T: Into<i64>>(value: T) -> Value {
    Value::Int(value.into())
}

fn integer_overflow(value: u64) -> String {
    format!("{value} does not fit a 64-bit signed integer")
}
