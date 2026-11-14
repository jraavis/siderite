//! Executor, backend and transaction traits implemented by database adapters.
//!
//! Application code does not use these traits directly; it goes through the
//! [`Db`](crate::Db) handle, which wraps either a backend (pool) or an open
//! transaction behind the same API.

use crate::capabilities::{BackendCapabilities, IsolationLevel};
use crate::cells::RowSink;
use crate::error::{OrmError, QueryError};
use crate::plan::QueryPlan;
use crate::value::Value;
use crate::write::WritePlan;
use async_trait::async_trait;
use std::sync::Arc;

/// One decoded result row: column names paired with backend-neutral values.
///
/// Dynamic projections and annotations decode into `Row`; models decode from
/// it with [`Model::from_row`](crate::Model::from_row).
///
/// The column names are reference counted, so every row of one result set
/// can share a single header (see [`Row::with_columns`]).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    names: Arc<[String]>,
    values: Vec<Value>,
}

impl Row {
    /// Build a row from `(column, value)` pairs.
    pub fn new(columns: Vec<(String, Value)>) -> Self {
        let (names, values): (Vec<_>, Vec<_>) = columns.into_iter().unzip();
        Self {
            names: names.into(),
            values,
        }
    }

    /// Build a row over a shared header: `values[i]` belongs to
    /// `columns[i]`.
    ///
    /// Backends decode a whole result set against one `columns` allocation
    /// instead of copying the names into every row.
    ///
    /// Args:
    ///     columns: Column names in projection order.
    ///     values: One value per column; a surplus on either side is dropped.
    ///
    /// Returns:
    ///     The row.
    pub fn with_columns(columns: Arc<[String]>, values: Vec<Value>) -> Self {
        let mut values = values;
        values.truncate(columns.len());
        Self {
            names: columns,
            values,
        }
    }

    /// Index of `column`, checking position `hint` before scanning.
    ///
    /// Generated decoders pass a field's declaration index, which is the
    /// column's position in a default projection, so the common lookup is
    /// one comparison.
    ///
    /// Args:
    ///     column: Column name to find.
    ///     hint: Position to try first; any value is safe.
    ///
    /// Returns:
    ///     The position of the first column with that name, if any.
    pub fn position(&self, column: &str, hint: usize) -> Option<usize> {
        let found = find_column(&self.names, column, hint)?;
        (found < self.values.len()).then_some(found)
    }

    /// The shared column header.
    pub(crate) fn names(&self) -> &Arc<[String]> {
        &self.names
    }

    /// Look up a value by column name.
    pub fn get(&self, column: &str) -> Option<&Value> {
        self.values.get(self.position(column, 0)?)
    }

    /// Move the value at `index` out of the row, leaving `NULL` behind.
    ///
    /// Args:
    ///     index: Column position, from [`position`](Self::position).
    ///
    /// Returns:
    ///     The value, or `None` when the row has no such column.
    pub fn take_at(&mut self, index: usize) -> Option<Value> {
        self.values
            .get_mut(index)
            .map(|value| std::mem::replace(value, Value::Null))
    }

    /// Name and value of the `index`-th column.
    pub fn column_at(&self, index: usize) -> Option<(&str, &Value)> {
        Some((self.names.get(index)?.as_str(), self.values.get(index)?))
    }

    /// Iterate over `(column, value)` pairs in projection order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.names.iter().map(String::as_str).zip(&self.values)
    }

    /// Consume into `(column, value)` pairs.
    pub fn into_columns(self) -> Vec<(String, Value)> {
        self.names.iter().cloned().zip(self.values).collect()
    }

    /// Number of columns.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether the row has no columns.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Index of `column` in `names`: position `hint` when it holds that name,
/// else the first column with it.
pub(crate) fn find_column(names: &[String], column: &str, hint: usize) -> Option<usize> {
    match names.get(hint) {
        Some(name) if name == column => Some(hint),
        _ => names.iter().position(|name| name == column),
    }
}

/// Result of executing a read plan.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QueryResult {
    /// Rows in backend order.
    pub rows: Vec<Row>,
}

/// Result of executing a write plan.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExecResult {
    /// Rows inserted, updated or deleted.
    pub rows_affected: u64,
    /// Rows produced by `RETURNING`, in statement order.
    pub returning: Vec<Row>,
}

/// Runs plans and raw statements on a pool or inside a transaction.
///
/// Implementations **must** check plans against [`capabilities`](Self::capabilities)
/// (`QueryPlan::check` / `WritePlan::check`) before any I/O and return a
/// capability error instead of ignoring unsupported features.
#[async_trait]
pub trait Executor: Send + Sync + 'static {
    /// Declared capabilities.
    fn capabilities(&self) -> BackendCapabilities;

    /// Execute a read plan.
    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError>;

    /// Execute a read plan, handing each row to `sink` in backend order.
    ///
    /// The queryset decodes models through this. A backend that can hand
    /// over its driver's rows directly, without building [`Row`]s, overrides
    /// it; the default runs [`fetch`](Self::fetch) and passes every row on.
    ///
    /// # Errors
    /// As [`fetch`](Self::fetch), or the first error `sink` returns.
    async fn fetch_each(&self, plan: &QueryPlan, sink: &mut dyn RowSink) -> Result<(), OrmError> {
        for mut row in self.fetch(plan).await?.rows {
            sink.push(&mut row)?;
        }
        Ok(())
    }

    /// Execute a write plan.
    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError>;

    /// Run raw SQL returning rows. `params` are bound, never interpolated.
    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError>;

    /// Run raw SQL returning the affected-row count. `params` are bound.
    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError>;

    /// Run a multi-statement script without parameters (DDL, migrations).
    async fn execute_script(&self, sql: &str) -> Result<(), OrmError>;
}

/// A connection pool that can open transactions.
#[async_trait]
pub trait Backend: Executor {
    /// Count compiled read parameters without database I/O.
    ///
    /// Args:
    ///     _plan: Complete read plan whose existing binds consume capacity.
    ///
    /// Returns:
    ///     Exact compiled bind count, or None when the backend cannot report
    ///     it. SQL prefetch requires this contract for bounded batching.
    ///
    /// # Errors
    /// Capability or plan compilation errors.
    fn read_parameter_count(&self, _plan: &QueryPlan) -> Result<Option<usize>, OrmError> {
        Ok(None)
    }

    /// Begin a transaction on a dedicated connection.
    ///
    /// `isolation` is validated against
    /// [`BackendCapabilities::isolation_levels`] before any I/O.
    async fn begin(
        &self,
        isolation: Option<IsolationLevel>,
    ) -> Result<Box<dyn Transaction>, OrmError>;

    /// Connection for a schema migration.
    ///
    /// When `transactional` is true this is a transaction, same as
    /// [`begin`](Self::begin) with no isolation override. SQLite turns
    /// `PRAGMA foreign_keys` off on that connection *before* `BEGIN` (the
    /// pragma is a no-op inside a transaction), runs `PRAGMA foreign_key_check`
    /// before commit, and restores the previous pragma value afterwards.
    /// Other backends ignore the SQLite-specific work.
    ///
    /// # Errors
    /// The default implementation has no way to hold a connection outside a
    /// transaction, so `transactional == false` fails with a capability error
    /// instead of silently handing back a transaction.
    async fn begin_schema(&self, transactional: bool) -> Result<Box<dyn Transaction>, OrmError> {
        if !transactional {
            return Err(QueryError::InvalidPlan(
                "this backend does not support a non-transactional schema connection".into(),
            )
            .into());
        }
        self.begin(None).await
    }
}

/// An open transaction.
///
/// Dropping a transaction without calling [`commit`](Self::commit) rolls it
/// back (the connection is returned to the pool in a clean state).
#[async_trait]
pub trait Transaction: Executor {
    /// Commit. Later calls on this transaction fail with
    /// [`QueryError::TransactionClosed`].
    async fn commit(&self) -> Result<(), OrmError>;

    /// Roll back. Later calls fail like after [`commit`](Self::commit).
    async fn rollback(&self) -> Result<(), OrmError>;
}
