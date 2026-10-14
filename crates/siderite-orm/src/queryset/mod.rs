//! [`QuerySet`]: a lazy, immutable query over one model.
//!
//! Builder methods consume and return the queryset and never perform I/O;
//! terminal methods (`all`, `first`, `get`, `count`, `exists`, `update`, ...)
//! run the compiled [`QueryPlan`] on the queryset's [`Db`]. Clone a queryset
//! to branch from a common base.
//!
//! Mistakes that can only be detected while building (an unknown annotation
//! name, a filter on a window function) are remembered and returned by the
//! next terminal method, so builder chains stay infallible.
//!
//! # Annotations
//!
//! [`annotate`](QuerySet::annotate) and [`alias`](QuerySet::alias) name an
//! expression; later `filter` / `order_by` / `project` calls refer to it with
//! `Expr::col("name")` and the expression is inlined (PostgreSQL does not
//! allow output aliases in `WHERE` / `HAVING`). A filter on an aggregate
//! annotation becomes `HAVING`; when the projection contains aggregates the
//! remaining projected columns become the `GROUP BY` and the model's default
//! ordering is dropped, as in Django.

mod combine;
mod cursor;
mod fetch;
mod related;
mod rows;
mod write;

pub use cursor::CursorPage;
pub use fetch::Page;
pub use related::{Prefetch, Relation};

use crate::db::Db;
use crate::error::QueryError;
use crate::expr::{Column, Expr, Ident};
use crate::model::Model;
use crate::plan::{DistinctMode, LockMode, OrderDirection, OrderExpr, QueryPlan, SelectExpr};
use related::{Prefetcher, RelNode};
use std::marker::PhantomData;
use std::sync::Arc;

/// Lazy query over model `M`.
pub struct QuerySet<M: Model> {
    db: Db,
    plan: QueryPlan,
    /// Named expressions from `annotate` / `alias`, already fully resolved.
    annotations: Vec<(Ident, Expr)>,
    /// The ordering still comes from `ModelMeta::ordering` (dropped for
    /// grouped queries).
    default_ordering: bool,
    /// `none()`: terminals return empty results without touching the database.
    empty: bool,
    /// Joined models loaded by `select_related`.
    related: Vec<RelNode>,
    /// Extra queries run by `prefetch_related`.
    prefetches: Vec<Arc<dyn Prefetcher<M>>>,
    /// The token set by `after` / `before`, read by `cursor_page`.
    cursor: Option<(cursor::Way, String)>,
    /// First problem found while building; returned by terminals.
    error: Option<QueryError>,
    _model: PhantomData<fn() -> M>,
}

impl<M: Model> Clone for QuerySet<M> {
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            plan: self.plan.clone(),
            annotations: self.annotations.clone(),
            default_ordering: self.default_ordering,
            empty: self.empty,
            related: self.related.clone(),
            prefetches: self.prefetches.clone(),
            cursor: self.cursor.clone(),
            error: self.error.clone(),
            _model: PhantomData,
        }
    }
}

impl<M: Model> std::fmt::Debug for QuerySet<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuerySet")
            .field("model", &M::META.name)
            .field("plan", &self.plan)
            .finish_non_exhaustive()
    }
}

impl<M: Model> QuerySet<M> {
    /// Every row of `M`'s table, in the model's default ordering.
    pub fn new(db: &Db) -> Self {
        let mut plan = QueryPlan::from_table(M::META.table);
        for (column, direction) in M::META.ordering {
            plan = plan.order_by(Expr::col(*column), *direction);
        }
        Self {
            db: db.clone(),
            plan,
            annotations: Vec::new(),
            default_ordering: !M::META.ordering.is_empty(),
            empty: false,
            related: Vec::new(),
            prefetches: Vec::new(),
            cursor: None,
            error: None,
            _model: PhantomData,
        }
    }

    /// The database handle this queryset runs on.
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The plan built so far.
    pub fn plan(&self) -> &QueryPlan {
        &self.plan
    }

    /// Run on `db` instead (Django `using`), e.g. a replica or the current
    /// transaction handle.
    #[must_use]
    pub fn using(mut self, db: &Db) -> Self {
        self.db = db.clone();
        self
    }

    /// Replace the plan (for extension code building on this queryset).
    #[must_use]
    pub fn with_plan(mut self, f: impl FnOnce(QueryPlan) -> QueryPlan) -> Self {
        self.plan = f(self.plan);
        self.settle()
    }

    /// Record the first building problem.
    fn fail(mut self, message: impl Into<String>) -> Self {
        self.error
            .get_or_insert_with(|| QueryError::InvalidPlan(message.into()));
        self
    }

    /// The building problem, if any; terminals call this first.
    fn ready(&self) -> Result<(), QueryError> {
        self.error.clone().map_or(Ok(()), Err)
    }

    /// Normalise the plan after an edit: turn related columns into joins and
    /// derive `GROUP BY` from the projection.
    fn settle(mut self) -> Self {
        let plan = std::mem::replace(&mut self.plan, QueryPlan::from_table(""));
        let mut plan = plan.resolve_relations();
        let aggregated = plan.having.is_some()
            || plan
                .projection
                .iter()
                .map(|s| &s.expr)
                .chain(plan.ordering.iter().map(|o| &o.expr))
                .any(Expr::contains_aggregate);
        if aggregated && plan.projection.is_empty() {
            plan.projection = model_projection::<M>();
        }
        plan.grouping = if aggregated {
            plan.projection
                .iter()
                .map(|s| &s.expr)
                .filter(|e| !e.contains_aggregate() && !e.contains_window())
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        if !plan.grouping.is_empty() && self.default_ordering {
            plan.ordering.clear();
        }
        self.plan = plan;
        self
    }

    /// Inline annotation references in `expr`.
    fn resolve(&self, mut expr: Expr) -> Expr {
        inline_annotations(&self.annotations, &mut expr);
        expr
    }

    /// Keep rows matching `predicate` (ANDed with earlier filters).
    ///
    /// A predicate over an aggregate annotation is applied as `HAVING`.
    #[must_use]
    pub fn filter(self, predicate: Expr) -> Self {
        let predicate = self.resolve(predicate);
        if predicate.contains_window() {
            return self.fail("cannot filter on a window function; wrap the query as a subquery");
        }
        self.with_plan(|mut plan| {
            if predicate.contains_aggregate() {
                plan.having = Some(match plan.having.take() {
                    Some(existing) => existing.and(predicate),
                    None => predicate,
                });
                plan
            } else {
                plan.filter(predicate)
            }
        })
    }

    /// Drop rows matching `predicate`.
    #[must_use]
    pub fn exclude(self, predicate: Expr) -> Self {
        self.filter(!predicate)
    }

    /// Replace the ordering (`order_by([User::name.asc(), User::id.desc()])`).
    #[must_use]
    pub fn order_by(mut self, terms: impl IntoIterator<Item = OrderExpr>) -> Self {
        let terms: Vec<OrderExpr> = terms
            .into_iter()
            .map(|term| OrderExpr {
                expr: self.resolve(term.expr),
                direction: term.direction,
            })
            .collect();
        self.default_ordering = false;
        self.with_plan(|mut plan| {
            plan.ordering = terms;
            plan
        })
    }

    /// Flip every ordering direction. An unordered queryset is ordered by
    /// primary key descending, so `reverse().first()` is the last row.
    #[must_use]
    pub fn reverse(mut self) -> Self {
        self.default_ordering = false;
        let pk = M::META.pk().map(|f| f.column);
        self.with_plan(|mut plan| {
            if plan.ordering.is_empty() {
                plan.ordering.extend(pk.map(|c| Expr::col(c).desc()));
                return plan;
            }
            for term in &mut plan.ordering {
                term.direction = match term.direction {
                    OrderDirection::Asc => OrderDirection::Desc,
                    OrderDirection::Desc => OrderDirection::Asc,
                };
            }
            plan
        })
    }

    /// Return at most `n` rows.
    #[must_use]
    pub fn limit(self, n: u64) -> Self {
        self.with_plan(|p| p.limit(n))
    }

    /// Skip the first `n` rows.
    #[must_use]
    pub fn offset(self, n: u64) -> Self {
        self.with_plan(|p| p.offset(n))
    }

    /// Remove duplicate rows (`SELECT DISTINCT`).
    #[must_use]
    pub fn distinct(self) -> Self {
        self.with_plan(|p| p.distinct(DistinctMode::All))
    }

    /// Keep the first row per distinct value of `exprs` (`DISTINCT ON`,
    /// PostgreSQL only; combine with a matching `order_by`).
    #[must_use]
    pub fn distinct_on<E: Into<Expr>>(self, exprs: impl IntoIterator<Item = E>) -> Self {
        let exprs = exprs.into_iter().map(|e| self.resolve(e.into())).collect();
        self.with_plan(|p| p.distinct(DistinctMode::On(exprs)))
    }

    /// A queryset that yields nothing and never queries the database.
    #[must_use]
    pub fn none(mut self) -> Self {
        self.empty = true;
        self
    }

    /// Name the root table in this query, so a subquery over the same table
    /// can refer to the enclosing row with [`Expr::outer`].
    #[must_use]
    pub fn aliased(self, alias: impl Into<Ident>) -> Self {
        let alias = alias.into();
        self.with_plan(|mut p| {
            p.source.alias = Some(alias);
            p
        })
    }

    /// Lock the selected rows (`SELECT .. FOR UPDATE`). Backends without row
    /// locking (SQLite) reject the query before running it. Locks are held
    /// until the surrounding transaction ends, so use this inside
    /// [`Db::transaction`].
    #[must_use]
    pub fn select_for_update(self) -> Self {
        self.with_plan(|p| p.lock(LockMode::ForUpdate))
    }

    /// `FOR UPDATE NOWAIT`: fail instead of waiting for locked rows.
    #[must_use]
    pub fn nowait(self) -> Self {
        self.with_plan(|p| p.lock(LockMode::ForUpdateNoWait))
    }

    /// `FOR UPDATE SKIP LOCKED`: skip rows locked by other transactions.
    #[must_use]
    pub fn skip_locked(self) -> Self {
        self.with_plan(|p| p.lock(LockMode::ForUpdateSkipLocked))
    }

    /// Add `expr` to the result as column `name` (Django `annotate`).
    ///
    /// Aggregates group by the other projected columns (see the module
    /// docs). Read the value with [`all_annotated`](Self::all_annotated) or
    /// [`rows`](Self::rows).
    #[must_use]
    pub fn annotate(self, name: impl Into<Ident>, expr: impl Into<Expr>) -> Self {
        self.name_expression(name.into(), expr.into(), true)
    }

    /// Name `expr` for use in `filter` / `order_by` without selecting it
    /// (Django `alias`).
    #[must_use]
    pub fn alias(self, name: impl Into<Ident>, expr: impl Into<Expr>) -> Self {
        self.name_expression(name.into(), expr.into(), false)
    }

    fn name_expression(mut self, name: Ident, expr: Expr, selected: bool) -> Self {
        if M::META.column(&name).is_some() || self.annotations.iter().any(|(n, _)| *n == name) {
            return self.fail(format!("annotation `{name}` clashes with an existing name"));
        }
        let expr = self.resolve(expr);
        self.annotations.push((name.clone(), expr.clone()));
        self.with_plan(|mut plan| {
            if selected {
                if plan.projection.is_empty() {
                    plan.projection = model_projection::<M>();
                }
                plan.projection.push(SelectExpr::new(expr, Some(name)));
            }
            plan
        })
    }
}

/// Every column of `M` as an explicit projection.
fn model_projection<M: Model>() -> Vec<SelectExpr> {
    M::META
        .fields
        .iter()
        .map(|f| SelectExpr::new(Expr::col(f.column), None))
        .collect()
}

/// Replace unqualified column references named like an annotation by the
/// annotated expression. Nested subqueries keep their own names.
fn inline_annotations(annotations: &[(Ident, Expr)], expr: &mut Expr) {
    if let Expr::Column(Column { source: None, name }) = expr
        && let Some((_, annotated)) = annotations.iter().find(|(n, _)| n == name)
    {
        *expr = annotated.clone();
        return;
    }
    for child in expr.children_mut() {
        inline_annotations(annotations, child);
    }
}

impl Db {
    /// Queryset over `M` on this handle (same as `M::objects(&db)`).
    pub fn objects<M: Model>(&self) -> QuerySet<M> {
        QuerySet::new(self)
    }
}
