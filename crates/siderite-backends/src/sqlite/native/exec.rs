//! Running statements on a connection: binding, stepping, decoding, and
//! where the work runs ([`Dispatch`]).
//!
//! Statement semantics follow SQLx's SQLite driver, so both backends answer
//! the same SQL the same way:
//!
//! - SQL holding several statements runs all of them. Anonymous `?`
//!   parameters are numbered across the statements; `?NNN` and `$NNN` name
//!   an absolute position. Missing parameters bind `NULL` and surplus ones
//!   are ignored.
//! - Every statement is stepped to the end, rows and all. The affected-row
//!   count is the sum of `sqlite3_changes()` after each statement.
//! - Fetched rows share the column header of the first row.
//! - Cells decode by storage class: `INTEGER` to [`Value::Int`], `REAL` to
//!   [`Value::Float`], `TEXT` to [`Value::Text`], `BLOB` to
//!   [`Value::Bytes`], `NULL` to [`Value::Null`].

use super::Dispatch;
use super::error;
use super::pool::Lease;
use crate::shared::{affected_result, returning_result};
use crate::sqlite::param::Stored;
use rusqlite::types::{Null, ValueRef};
use rusqlite::{Batch, Connection, Statement};
use siderite_orm::{BackendError, Cells, ExecResult, OrmError, QueryError, QueryResult, Row};
use siderite_orm::{RowSink, Value};
use std::sync::Arc;

/// One operation on a connection: SQL and its parameters in, `T` out.
pub(super) type Run<T> = fn(&Connection, &str, Vec<Value>) -> Result<T, OrmError>;

/// Run `run` on `lease`'s connection where `mode` says, then hand the lease
/// back.
pub(super) async fn dispatch<T: Send + 'static>(
    mode: Dispatch,
    lease: Lease,
    sql: &str,
    params: Vec<Value>,
    run: Run<T>,
) -> Result<T, OrmError> {
    match mode {
        Dispatch::Inline => run(lease.conn()?, sql, params),
        Dispatch::Blocking => {
            let sql = sql.to_owned();
            blocking(move || run(lease.conn()?, &sql, params)).await
        }
    }
}

/// Run `work` on Tokio's blocking pool. A panic resumes on the caller.
pub(super) async fn blocking<T, F>(work: F) -> Result<T, OrmError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, OrmError> + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(done) => done,
        Err(failure) => match failure.try_into_panic() {
            Ok(panic) => std::panic::resume_unwind(panic),
            Err(failure) => Err(BackendError::Database(format!(
                "SQLite blocking task did not finish: {failure}"
            ))
            .into()),
        },
    }
}

/// Fetch every row `sql` produces.
pub(super) fn fetch_rows(
    conn: &Connection,
    sql: &str,
    params: Vec<Value>,
) -> Result<QueryResult, OrmError> {
    let mut rows = Collect::default();
    statements(conn, sql, &stored(params), true, |row| rows.push(row))?;
    Ok(QueryResult { rows: rows.rows })
}

/// Run `sql`, handing every row `sql` produces to `sink` straight from the
/// driver, without building [`Row`]s.
pub(super) fn fetch_into(
    conn: &Connection,
    sql: &str,
    params: Vec<Value>,
    sink: &mut dyn RowSink,
) -> Result<(), OrmError> {
    let mut names = None;
    statements(conn, sql, &stored(params), true, |row| {
        let names = shared_header(&mut names, row)?;
        sink.push(&mut DriverCells { names, row })
    })
    .map(drop)
}

/// [`fetch_into`] where `mode` says. Off the calling task the sink cannot
/// follow, so the rows are fetched there and handed over afterwards.
pub(super) async fn dispatch_into(
    mode: Dispatch,
    lease: Lease,
    sql: &str,
    params: Vec<Value>,
    sink: &mut dyn RowSink,
) -> Result<(), OrmError> {
    match mode {
        Dispatch::Inline => fetch_into(lease.conn()?, sql, params, sink),
        Dispatch::Blocking => {
            for mut row in dispatch(mode, lease, sql, params, fetch_rows).await?.rows {
                sink.push(&mut row)?;
            }
            Ok(())
        }
    }
}

/// Run `sql` and return the number of rows it changed.
pub(super) fn execute_rows(
    conn: &Connection,
    sql: &str,
    params: Vec<Value>,
) -> Result<u64, OrmError> {
    statements(conn, sql, &stored(params), true, |_| Ok(()))
}

/// Run a script (several statements, nothing cached).
pub(super) fn script(conn: &Connection, sql: &str, params: Vec<Value>) -> Result<(), OrmError> {
    statements(conn, sql, &stored(params), false, |_| Ok(())).map(drop)
}

/// The write runner: `RETURNING` rows are the outcome when there are any.
pub(super) fn write_fn(returning: bool) -> Run<ExecResult> {
    if returning {
        write_returning
    } else {
        write_affected
    }
}

fn write_returning(
    conn: &Connection,
    sql: &str,
    params: Vec<Value>,
) -> Result<ExecResult, OrmError> {
    Ok(returning_result(fetch_rows(conn, sql, params)?.rows))
}

fn write_affected(
    conn: &Connection,
    sql: &str,
    params: Vec<Value>,
) -> Result<ExecResult, OrmError> {
    Ok(affected_result(execute_rows(conn, sql, params)?))
}

/// Run a parameterless script and drop its rows, reporting driver errors
/// as they are (connection setup).
pub(super) fn run_script(conn: &Connection, sql: &str) -> rusqlite::Result<()> {
    let mut batch = Batch::new(conn, sql);
    while let Some(mut statement) = batch.next()? {
        let mut rows = statement.raw_query();
        while rows.next()?.is_some() {}
    }
    Ok(())
}

fn stored(params: Vec<Value>) -> Vec<Stored> {
    params.into_iter().map(Stored::from).collect()
}

/// Run every statement in `sql`, handing each row to `on_row`, and return
/// the summed change count.
///
/// SQL that is a single statement goes through the connection's statement
/// cache when `cache` is set. Anything else (several statements, or a bare
/// comment) is split with [`Batch`] and prepared each time.
///
/// A row `on_row` rejects does not stop the statements, which run to the end
/// as they would without the rejection; the first rejection is returned
/// afterwards.
fn statements(
    conn: &Connection,
    sql: &str,
    params: &[Stored],
    cache: bool,
    mut on_row: impl FnMut(&rusqlite::Row<'_>) -> Result<(), QueryError>,
) -> Result<u64, OrmError> {
    let mut rejected = None;
    let mut visit = |row: &rusqlite::Row<'_>| {
        if rejected.is_none()
            && let Err(err) = on_row(row)
        {
            rejected = Some(err);
        }
    };
    let changes = if cache && single_statement(sql) {
        let mut statement = conn.prepare_cached(sql).map_err(error::map)?;
        bind(&mut statement, params, 0)?;
        step(&mut statement, &mut visit)?;
        conn.changes()
    } else {
        let mut batch = Batch::new(conn, sql);
        let (mut used, mut changes) = (0, 0);
        while let Some(mut statement) = batch.next().map_err(error::map)? {
            used += bind(&mut statement, params, used)?;
            step(&mut statement, &mut visit)?;
            changes += conn.changes();
        }
        changes
    };
    match rejected {
        Some(err) => Err(err.into()),
        None => Ok(changes),
    }
}

/// Whether `sql` is exactly one statement, so the statement cache applies.
/// Errs towards `false`: a `;` inside a literal only costs the cache.
fn single_statement(sql: &str) -> bool {
    let body = sql
        .trim_end_matches(|c: char| c == ';' || c.is_ascii_whitespace())
        .trim_start();
    !body.is_empty() && !body.contains(';') && !body.starts_with("--") && !body.starts_with("/*")
}

fn step(
    statement: &mut Statement<'_>,
    visit: &mut impl FnMut(&rusqlite::Row<'_>),
) -> Result<(), OrmError> {
    let mut rows = statement.raw_query();
    while let Some(row) = rows.next().map_err(error::map)? {
        visit(row);
    }
    Ok(())
}

/// Bind `params` to `statement`'s placeholders the way SQLx does, starting
/// anonymous placeholders after `offset` values. Returns how many anonymous
/// positions the statement consumed.
fn bind(
    statement: &mut Statement<'_>,
    params: &[Stored],
    offset: usize,
) -> Result<usize, OrmError> {
    let mut next = offset;
    for index in 1..=statement.parameter_count() {
        let position = match statement.parameter_name(index) {
            None => {
                next += 1;
                next
            }
            Some(name) => named_position(name)?,
        };
        // Missing values stay unbound, which SQLite reads as NULL.
        let Some(value) = position.checked_sub(1).and_then(|at| params.get(at)) else {
            break;
        };
        let bound = match value {
            Stored::Null => statement.raw_bind_parameter(index, Null),
            Stored::Integer(i) => statement.raw_bind_parameter(index, i),
            Stored::Real(f) => statement.raw_bind_parameter(index, f),
            Stored::Text(s) => statement.raw_bind_parameter(index, s.as_str()),
            Stored::Blob(b) => statement.raw_bind_parameter(index, b.as_slice()),
        };
        bound.map_err(error::map)?;
    }
    Ok(next - offset)
}

/// The 1-based value position a `?NNN` or `$NNN` placeholder names.
fn named_position(name: &str) -> Result<usize, OrmError> {
    let number = |digits: &str| -> usize {
        let end = digits
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(digits.len());
        digits.get(..end).and_then(|n| n.parse().ok()).unwrap_or(0)
    };
    if let Some(digits) = name.strip_prefix('?') {
        return Ok(number(digits));
    }
    let message = match name.strip_prefix('$') {
        Some(digits) if digits.starts_with(|c: char| c.is_ascii_digit()) => {
            return Ok(number(digits));
        }
        Some(digits) => {
            format!("parameters with non-integer names are not currently supported: {digits}")
        }
        None => format!("unsupported SQL parameter format: {name}"),
    };
    Err(BackendError::Database(format!("encountered unexpected or invalid data: {message}")).into())
}

/// Rows decoded against one shared header.
#[derive(Default)]
struct Collect {
    names: Option<Arc<[String]>>,
    rows: Vec<Row>,
}

impl Collect {
    fn push(&mut self, row: &rusqlite::Row<'_>) -> Result<(), QueryError> {
        let names = shared_header(&mut self.names, row)?;
        self.rows.push(DriverCells { names, row }.to_row()?);
        Ok(())
    }
}

/// The header every row of a fetch shares: built from the first row.
fn shared_header<'a>(
    names: &'a mut Option<Arc<[String]>>,
    row: &rusqlite::Row<'_>,
) -> Result<&'a Arc<[String]>, QueryError> {
    match names {
        Some(names) => Ok(names),
        None => Ok(names.insert(header(row)?)),
    }
}

/// A driver row read against the fetch's shared header.
struct DriverCells<'a, 'stmt> {
    names: &'a Arc<[String]>,
    row: &'a rusqlite::Row<'stmt>,
}

impl Cells for DriverCells<'_, '_> {
    fn columns(&self) -> &Arc<[String]> {
        self.names
    }

    fn take(&mut self, index: usize, out: &mut Value) -> Result<bool, QueryError> {
        let Some(name) = self.names.get(index) else {
            return Ok(false);
        };
        *out = cell(self.row, index).map_err(|reason| QueryError::Decode {
            column: name.clone(),
            reason,
        })?;
        Ok(true)
    }

    fn to_row(&mut self) -> Result<Row, QueryError> {
        let values = (0..self.names.len())
            .map(|index| {
                let mut value = Value::Null;
                self.take(index, &mut value)?;
                Ok(value)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Row::with_columns(Arc::clone(self.names), values))
    }
}

/// The column names of `row`'s statement.
fn header(row: &rusqlite::Row<'_>) -> Result<Arc<[String]>, QueryError> {
    let statement = row.as_ref();
    (0..statement.column_count())
        .map(|index| {
            statement
                .column_name(index)
                .map(str::to_owned)
                .map_err(|e| QueryError::Decode {
                    column: index.to_string(),
                    reason: e.to_string(),
                })
        })
        .collect()
}

/// Decode one cell by its storage class.
fn cell(row: &rusqlite::Row<'_>, index: usize) -> Result<Value, String> {
    Ok(match row.get_ref(index).map_err(|e| e.to_string())? {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Int(i),
        ValueRef::Real(f) => Value::Float(f),
        ValueRef::Text(text) => Value::Text(
            std::str::from_utf8(text)
                .map_err(|e| e.to_string())?
                .to_owned(),
        ),
        ValueRef::Blob(bytes) => Value::Bytes(bytes.to_vec()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_statement_detection() {
        assert!(single_statement("SELECT 1"));
        assert!(single_statement("  SELECT 1 ;  \n"));
        assert!(single_statement("SELECT 1 -- trailing"));
        assert!(!single_statement("SELECT 1; SELECT 2"));
        assert!(!single_statement("-- only a comment"));
        assert!(!single_statement("/* only */"));
        assert!(!single_statement("   "));
    }

    #[test]
    fn named_positions_follow_sqlx() {
        assert_eq!(named_position("?3").ok(), Some(3));
        assert_eq!(named_position("$12").ok(), Some(12));
        let named = named_position(":name").map_err(|e| e.to_string());
        assert_eq!(
            named,
            Err("database error: encountered unexpected or invalid data: \
                 unsupported SQL parameter format: :name"
                .into())
        );
        assert!(named_position("$abc").is_err());
    }
}
