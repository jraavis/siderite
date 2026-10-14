//! Cursor (keyset) pagination: [`QuerySet::after`], [`QuerySet::before`]
//! and [`QuerySet::cursor_page`].
//!
//! A page is one statement: the rows past the cursor in the queryset's
//! ordering plus one extra row, which only tells whether more rows follow.
//! The cursor holds the ordering values of a row and a fingerprint of the
//! queryset, so a token only fits the list it came from. See
//! `docs/plans/CURSOR_PAGINATION.md` for the semantics.

mod seek;
mod token;

use super::QuerySet;
use crate::backend::Row;
use crate::capabilities::Feature;
use crate::error::{OrmError, QueryError};
use crate::model::Model;
use crate::value::Value;
use seek::{Seek, Term};
use token::Slot;

/// The direction a cursor leads in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Way {
    /// Rows after the cursor (`after`).
    After,
    /// Rows before the cursor (`before`).
    Before,
}

impl Way {
    /// The byte that marks the direction inside a token.
    fn byte(self) -> u8 {
        match self {
            Self::After => b'a',
            Self::Before => b'b',
        }
    }
}

/// One page of a [`QuerySet::cursor_page`] result.
///
/// Pass [`next`](Self::next) to [`QuerySet::after`] for the following page
/// and [`previous`](Self::previous) to [`QuerySet::before`] for the one
/// before. There is no total: counting is a separate query.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorPage<M> {
    /// Rows of this page, in the queryset's ordering.
    pub items: Vec<M>,
    /// Cursor for the page after this one; `None` on the last page.
    pub next: Option<String>,
    /// Cursor for the page before this one; `None` on the first page.
    pub previous: Option<String>,
}

impl<M> CursorPage<M> {
    fn empty() -> Self {
        Self {
            items: Vec::new(),
            next: None,
            previous: None,
        }
    }
}

/// The result of [`QuerySet::cursor_page`].
type Fetched<M> = Result<CursorPage<M>, OrmError>;

/// A token, or `None` where the page has no such neighbour.
type Token = Result<Option<String>, QueryError>;

fn missing(column: &str) -> QueryError {
    QueryError::Decode {
        column: column.to_owned(),
        reason: "column missing from result".into(),
    }
}

/// Everything needed to turn row positions into tokens.
struct Cursors<'a> {
    terms: &'a [Term],
    fingerprint: u64,
}

impl Cursors<'_> {
    /// The ordering values of `row`, in canonical form.
    fn values(&self, row: &Row) -> Result<Vec<Value>, QueryError> {
        self.terms
            .iter()
            .map(|term| {
                let column = term.field.column;
                let raw = row.get(column).cloned();
                let raw = raw.ok_or_else(|| missing(column))?;
                seek::canonical(column, term.field.sql_type, raw)
            })
            .collect()
    }

    /// The token for a position, if there is one.
    fn token(&self, way: Way, values: Option<&[Value]>) -> Token {
        let encode = |v: &[Value]| token::encode(self.fingerprint, way, v);
        values.map(encode).transpose()
    }
}

impl<M: Model> QuerySet<M> {
    /// Continue after the position `token` names (a [`CursorPage::next`]).
    ///
    /// The last of `after` and [`before`](Self::before) wins only within
    /// the same direction; asking for both is an error. The token is
    /// checked by [`cursor_page`](Self::cursor_page).
    #[must_use]
    pub fn after(self, token: &str) -> Self {
        self.with_cursor(Way::After, token)
    }

    /// Page backwards from the position `token` names (a
    /// [`CursorPage::previous`]). See [`after`](Self::after).
    #[must_use]
    pub fn before(self, token: &str) -> Self {
        self.with_cursor(Way::Before, token)
    }

    fn with_cursor(mut self, way: Way, token: &str) -> Self {
        if self.cursor.as_ref().is_some_and(|(set, _)| *set != way) {
            return self.fail("`after` and `before` cannot be combined");
        }
        self.cursor = Some((way, token.to_owned()));
        self
    }

    /// One page of at most `limit` rows (at least 1) in a single
    /// statement, without a count.
    ///
    /// The ordering is the queryset's `order_by` (or the model's), made
    /// total by appending the primary key. Its terms must be columns of
    /// the model. Nulls sort last ascending and first descending on every
    /// backend. A cursor set with [`after`](Self::after) or
    /// [`before`](Self::before) is validated against this queryset's
    /// fingerprint: its filters, ordering and table.
    ///
    /// # Errors
    /// [`QueryError::InvalidPlan`] for `limit == 0`, a queryset with
    /// `distinct`, grouping, set operations, `limit`/`offset` or row
    /// locks, or an ordering term that is not a model column;
    /// [`OrmError::Cursor`] for a rejected token; capability errors
    /// (Redis, and MongoDB with a nullable ordering column) and backend
    /// errors.
    pub async fn cursor_page(self, limit: u64) -> Fetched<M> {
        if limit == 0 {
            let err = QueryError::InvalidPlan("limit starts at 1".into());
            return Err(err.into());
        }
        self.ready()?;
        if self.empty {
            return Ok(CursorPage::empty());
        }
        let caps = self.db.capabilities();
        caps.require(Feature::KeysetPagination)?;
        self.plan.check(&caps)?;
        seek::check_shape(&self.plan)?;
        let terms = seek::terms::<M>(&self.plan)?;
        if terms.iter().any(|t| t.field.nullable) {
            caps.require(Feature::NullableKeyset)?;
        }
        let print = seek::fingerprint(&self.plan, &terms);
        let cursors = Cursors {
            terms: &terms,
            fingerprint: print,
        };
        let slots: Vec<Slot> = terms.iter().map(Term::slot).collect();
        let (way, at) = match &self.cursor {
            None => (Way::After, None),
            Some((way, raw)) => {
                let values = token::decode(raw, print, *way, &slots)?;
                (*way, Some(values))
            }
        };
        let backward = way == Way::Before;
        let seek = Seek::new(&self.plan, &terms).backwards(backward);
        let order = seek.ordering();
        let predicate = at.as_deref().map(|values| seek.predicate(values));
        let paged = self.with_plan(|mut plan| {
            plan.ordering = order;
            if let Some(predicate) = predicate {
                plan = plan.filter(predicate);
            }
            plan.limit(limit.saturating_add(1))
        });
        let mut rows = paged.db.fetch(&paged.plan).await?.rows;
        let size = usize::try_from(limit).unwrap_or(usize::MAX);
        let more = rows.len() > size;
        rows.truncate(size);
        if backward {
            rows.reverse();
        }
        let first = rows.first().map(|row| cursors.values(row)).transpose()?;
        let last = rows.last().map(|row| cursors.values(row)).transpose()?;
        let items = paged
            .decode_rows(rows)
            .await?
            .into_iter()
            .map(|(model, _)| model)
            .collect();
        let (next, previous) = if backward {
            let previous = if more { first.as_deref() } else { None };
            let next = last.as_deref().or(at.as_deref());
            (next, previous)
        } else {
            let next = if more { last.as_deref() } else { None };
            let previous = at.as_ref().and(first.as_deref().or(at.as_deref()));
            (next, previous)
        };
        let next = cursors.token(Way::After, next)?;
        let previous = cursors.token(Way::Before, previous)?;
        Ok(CursorPage {
            items,
            next,
            previous,
        })
    }
}

#[cfg(test)]
mod tests;
