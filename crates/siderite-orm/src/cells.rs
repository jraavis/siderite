//! Decoding models from result rows without a by-name lookup per row.
//!
//! Every row of one result set shares its column header, so where a model's
//! columns sit is resolved once per header into a [`ColumnPlan`]; each row
//! then reads its cells by index. Rows reach a model through [`Cells`],
//! which a materialized [`Row`] implements and which a backend can
//! implement over its driver's row to skip building `Row`s altogether
//! (see [`Executor::fetch_each`](crate::Executor::fetch_each)).

use crate::backend::{Row, find_column};
use crate::error::QueryError;
use crate::model::{Model, ModelMeta, column_key, missing};
use crate::types::{DbType, decode};
use crate::value::Value;
use std::borrow::Cow;
use std::sync::Arc;

/// The cells of one result row, read by column index.
pub trait Cells {
    /// The column header shared by every row of the result set.
    fn columns(&self) -> &Arc<[String]>;

    /// Move the value at `index` out of the row into `out`.
    ///
    /// Each cell is meant to be taken once; what a second take of the same
    /// cell returns (the value again, or `NULL`) depends on the row. The
    /// value is written into `out` rather than returned so it is not
    /// copied through a wrapping `Result` on every cell.
    ///
    /// Args:
    ///     index: Column position in [`columns`](Self::columns).
    ///     out: Receives the value; untouched when there is no such column.
    ///
    /// Returns:
    ///     Whether the row has that column.
    ///
    /// # Errors
    /// [`QueryError::Decode`] naming the column when the driver cannot
    /// decode the cell.
    fn take(&mut self, index: usize, out: &mut Value) -> Result<bool, QueryError>;

    /// The row as a [`Row`], for decoders that need one. Call it before
    /// taking any cell.
    ///
    /// # Errors
    /// As [`take`](Self::take).
    fn to_row(&mut self) -> Result<Row, QueryError>;
}

impl Cells for Row {
    fn columns(&self) -> &Arc<[String]> {
        self.names()
    }

    fn take(&mut self, index: usize, out: &mut Value) -> Result<bool, QueryError> {
        Ok(match self.take_at(index) {
            Some(value) => {
                *out = value;
                true
            }
            None => false,
        })
    }

    fn to_row(&mut self) -> Result<Row, QueryError> {
        Ok(std::mem::take(self))
    }
}

/// Receives rows one at a time from [`Executor::fetch_each`](crate::Executor::fetch_each).
pub trait RowSink: Send {
    /// Consume one row.
    ///
    /// # Errors
    /// Any decode error; the fetch reports the first one.
    fn push(&mut self, cells: &mut dyn Cells) -> Result<(), QueryError>;
}

/// Where a model's columns sit in one result header.
///
/// Positions follow [`Row::position`]: field `i` of
/// [`ModelMeta::fields`] is looked for at index `i` first, then as the
/// first column with its (prefixed) name.
#[derive(Debug, Clone)]
pub struct ColumnPlan {
    columns: Arc<[String]>,
    prefix: Box<str>,
    slots: Vec<(Option<usize>, Cow<'static, str>)>,
}

impl ColumnPlan {
    /// Resolve `meta`'s columns, named `prefix + column`, in `columns`.
    ///
    /// Args:
    ///     meta: Model whose [`fields`](ModelMeta::fields) to place.
    ///     columns: Result header.
    ///     prefix: Column name prefix (`""` for the root model).
    ///
    /// Returns:
    ///     The plan; columns the header lacks fail when a row is decoded.
    pub fn new(meta: &'static ModelMeta, columns: &Arc<[String]>, prefix: &str) -> Self {
        let slots = meta
            .fields
            .iter()
            .enumerate()
            .map(|(hint, field)| {
                let key = column_key(prefix, field.column);
                (find_column(columns, &key, hint), key)
            })
            .collect();
        Self {
            columns: Arc::clone(columns),
            prefix: prefix.into(),
            slots,
        }
    }

    /// Whether this plan was resolved against `columns` (the same header
    /// allocation, not just equal names).
    pub fn is_for(&self, columns: &Arc<[String]>) -> bool {
        Arc::ptr_eq(&self.columns, columns)
    }

    /// The column name prefix.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }
}

/// Move `column` out of `cells` at the position `plan` resolved for it,
/// decoding it as `T`.
///
/// Args:
///     cells: Row to take the value from.
///     plan: Positions resolved for this row's header.
///     column: Column name without the prefix.
///     field: Index of the column's field in [`ModelMeta::fields`].
///
/// Returns:
///     The decoded value.
///
/// # Errors
/// [`QueryError::Decode`] if the column is missing or has the wrong type,
/// named and worded as [`take_column`](crate::take_column) does.
pub fn take_planned<T: DbType>(
    cells: &mut dyn Cells,
    plan: &ColumnPlan,
    column: &str,
    field: usize,
) -> Result<T, QueryError> {
    let Some((index, key)) = plan.slots.get(field) else {
        return Err(missing(&column_key(plan.prefix(), column)));
    };
    let mut value = Value::Null;
    let found = match index {
        Some(index) => cells.take(*index, &mut value)?,
        None => false,
    };
    if !found {
        return Err(missing(key));
    }
    decode(key, value)
}

/// Collects the models of one result set, resolving a [`ColumnPlan`] once
/// per header.
pub(crate) struct ModelSink<M> {
    plan: Option<ColumnPlan>,
    models: Vec<M>,
}

impl<M: Model> ModelSink<M> {
    pub(crate) fn new() -> Self {
        Self {
            plan: None,
            models: Vec::new(),
        }
    }

    pub(crate) fn into_models(self) -> Vec<M> {
        self.models
    }
}

impl<M: Model> RowSink for ModelSink<M> {
    fn push(&mut self, cells: &mut dyn Cells) -> Result<(), QueryError> {
        let plan = match &mut self.plan {
            Some(plan) if plan.is_for(cells.columns()) => plan,
            slot => slot.insert(ColumnPlan::new(M::META, cells.columns(), "")),
        };
        self.models.push(M::from_cells(cells, plan)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
