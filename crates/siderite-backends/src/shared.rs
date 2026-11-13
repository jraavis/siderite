//! Pieces shared by the SQLx-based adapters ([`sqlite`](crate::sqlite),
//! [`postgres`](crate::postgres)): error mapping, write-result shaping and
//! the open-transaction slot.

use siderite_orm::{BackendError, ExecResult, OrmError, QueryError};
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use siderite_orm::{Row, Value};
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use sqlx::{Column as _, ValueRef as _};
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
use std::sync::Arc;
use tokio::sync::Mutex;

/// Classify a driver error: constraint violations (unique, foreign key,
/// check, not null) become [`BackendError::Constraint`], everything else
/// [`BackendError::Database`].
pub(crate) fn map_error(e: sqlx::Error) -> BackendError {
    match &e {
        sqlx::Error::Database(db)
            if db.constraint().is_some()
                || db.is_unique_violation()
                || db.is_foreign_key_violation()
                || db.is_check_violation() =>
        {
            BackendError::Constraint(db.message().to_owned())
        }
        _ => BackendError::Database(e.to_string()),
    }
}

/// Wire value of one cell of `R`.
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
type Cell<'r, R> = <Driver<R> as sqlx::Database>::ValueRef<'r>;

/// Database of the driver row `R`.
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
type Driver<R> = <R as sqlx::Row>::Database;

/// Rows decoded from one result set.
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
type Decoded = Result<Vec<Row>, QueryError>;

/// Decode a driver result set into rows that share one column header.
///
/// The column names are copied once for the whole set, not once per row.
///
/// Args:
///     rows: Driver rows of a single statement.
///     cell: Decodes the non-null cell at a column index of a row; the
///         error is the reason reported for that column.
///
/// Returns:
///     The decoded rows in driver order, or the first decode error, naming
///     its column.
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]
pub(crate) fn decode_rows<R, F>(rows: &[R], cell: F) -> Decoded
where
    R: sqlx::Row,
    usize: sqlx::ColumnIndex<R>,
    F: for<'r> Fn(&'r R, usize, Cell<'r, R>) -> Result<Value, String>,
{
    let Some(first) = rows.first() else {
        return Ok(Vec::new());
    };
    let names: Arc<[String]> = first
        .columns()
        .iter()
        .map(|column| column.name().to_owned())
        .collect();
    let decode = |row: &R, index: usize| {
        let raw = row.try_get_raw(index).map_err(|e| e.to_string())?;
        if raw.is_null() {
            Ok(Value::Null)
        } else {
            cell(row, index, raw)
        }
    };
    rows.iter()
        .map(|row| {
            (0..names.len())
                .map(|index| {
                    decode(row, index).map_err(|reason| QueryError::Decode {
                        column: names[index].clone(),
                        reason,
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|values| Row::with_columns(Arc::clone(&names), values))
        })
        .collect()
}

/// Result of a write that used `RETURNING`: the rows are the outcome.
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub(crate) fn returning_result(rows: Vec<Row>) -> ExecResult {
    ExecResult {
        rows_affected: rows.len() as u64,
        returning: rows,
    }
}

/// Result of a write without `RETURNING`.
pub(crate) fn affected_result(rows_affected: u64) -> ExecResult {
    ExecResult {
        rows_affected,
        returning: Vec::new(),
    }
}

/// The transaction of a [`Transaction`](siderite_orm::Transaction) adapter.
/// Empty once committed or rolled back, so later use fails cleanly; dropping
/// it while open rolls the transaction back.
pub(crate) struct TxSlot<DB: sqlx::Database> {
    pub(crate) tx: Mutex<Option<sqlx::Transaction<'static, DB>>>,
}

impl<DB: sqlx::Database> TxSlot<DB> {
    pub(crate) fn new(tx: sqlx::Transaction<'static, DB>) -> Self {
        Self {
            tx: Mutex::new(Some(tx)),
        }
    }

    async fn take(&self) -> Result<sqlx::Transaction<'static, DB>, QueryError> {
        self.tx
            .lock()
            .await
            .take()
            .ok_or(QueryError::TransactionClosed)
    }

    pub(crate) async fn commit(&self) -> Result<(), OrmError> {
        let tx = self.take().await?;
        tx.commit().await.map_err(|e| map_error(e).into())
    }

    pub(crate) async fn rollback(&self) -> Result<(), OrmError> {
        let tx = self.take().await?;
        tx.rollback().await.map_err(|e| map_error(e).into())
    }
}

/// Run `$body` with `$conn` bound to the open transaction's connection.
macro_rules! with_tx {
    ($slot:expr, $conn:ident => $body:expr) => {{
        let mut guard = $slot.tx.lock().await;
        let tx = guard
            .as_mut()
            .ok_or(::siderite_orm::QueryError::TransactionClosed)?;
        let $conn = &mut **tx;
        $body
    }};
}
pub(crate) use with_tx;

/// A pooled connection held without an SQL transaction (migration lock).
/// Dropping it without [`take`](Self::take) closes the connection so session
/// locks (`pg_advisory_lock`, `GET_LOCK`) are not returned to the pool.
#[cfg(any(feature = "postgres", feature = "mysql"))]
pub(crate) struct ConnSlot<DB: sqlx::Database> {
    pub(crate) conn: Mutex<Option<sqlx::pool::PoolConnection<DB>>>,
}

#[cfg(any(feature = "postgres", feature = "mysql"))]
impl<DB: sqlx::Database> ConnSlot<DB> {
    pub(crate) fn new(conn: sqlx::pool::PoolConnection<DB>) -> Self {
        Self {
            conn: Mutex::new(Some(conn)),
        }
    }

    pub(crate) async fn take(&self) -> Result<sqlx::pool::PoolConnection<DB>, QueryError> {
        self.conn
            .lock()
            .await
            .take()
            .ok_or(QueryError::TransactionClosed)
    }
}

#[cfg(any(feature = "postgres", feature = "mysql"))]
impl<DB: sqlx::Database> Drop for ConnSlot<DB> {
    fn drop(&mut self) {
        if let Some(mut conn) = self.conn.get_mut().take() {
            conn.close_on_drop();
        }
    }
}

#[cfg(any(feature = "postgres", feature = "mysql"))]
macro_rules! with_conn {
    ($slot:expr, $conn:ident => $body:expr) => {{
        let mut guard = $slot.conn.lock().await;
        let conn = guard
            .as_mut()
            .ok_or(::siderite_orm::QueryError::TransactionClosed)?;
        let $conn = &mut **conn;
        $body
    }};
}
#[cfg(any(feature = "postgres", feature = "mysql"))]
pub(crate) use with_conn;
