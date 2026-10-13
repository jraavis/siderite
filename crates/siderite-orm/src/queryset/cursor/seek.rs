//! The ordering of a cursor page, the fingerprint of its queryset and the
//! seek predicate, built from the plain expression builders.

use super::token::{Slot, tag_for};
use crate::error::QueryError;
use crate::expr::{Column, Expr, Ident};
use crate::model::{FieldMeta, Model};
use crate::plan::OrderDirection::{self, Asc, Desc};
use crate::plan::{DistinctMode, OrderExpr, QueryPlan};
use crate::types::{DbType, SqlType};
use crate::value::Value;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// One ordering term: a column of the model and a direction.
#[derive(Debug, Clone, Copy)]
pub(super) struct Term {
    pub(super) field: &'static FieldMeta,
    pub(super) direction: OrderDirection,
}

impl Term {
    pub(super) fn slot(&self) -> Slot {
        Slot {
            sql_type: self.field.sql_type,
            nullable: self.field.nullable,
        }
    }

    /// The direction on the query: inverted when paging backwards.
    fn way(&self, backward: bool) -> OrderDirection {
        match (self.direction, backward) {
            (Asc, false) | (Desc, true) => Asc,
            _ => Desc,
        }
    }
}

/// Result of building a part of a cursor page.
type Built<T> = Result<T, QueryError>;

fn invalid(message: &str) -> QueryError {
    QueryError::InvalidPlan(message.to_owned())
}

const COLUMNS: &str = "cursor ordering must use model columns";

/// Reject querysets whose pages a seek cannot express.
pub(super) fn check_shape(plan: &QueryPlan) -> Built<()> {
    let problem = if plan.distinct != DistinctMode::None {
        "distinct"
    } else if !plan.grouping.is_empty() || plan.having.is_some() {
        "grouping"
    } else if !plan.compound.is_empty() {
        "a set operation"
    } else if plan.limit.is_some() || plan.offset.is_some() {
        "limit or offset"
    } else if plan.lock.is_some() {
        "a row lock"
    } else if plan.source.subquery.is_some() {
        "a derived table"
    } else {
        return Ok(());
    };
    Err(QueryError::InvalidPlan(format!(
        "cursor pagination does not support {problem}"
    )))
}

/// The ordering terms of `plan`, with the primary key appended ascending
/// unless it is already one of them.
pub(super) fn terms<M: Model>(plan: &QueryPlan) -> Built<Vec<Term>> {
    let root = plan.source.reference();
    let mut out = Vec::with_capacity(plan.ordering.len() + 1);
    for order in &plan.ordering {
        let Expr::Column(column) = &order.expr else {
            return Err(invalid(COLUMNS));
        };
        if column.source.as_ref().is_some_and(|s| s != root) {
            return Err(invalid(COLUMNS));
        }
        let field = M::META.column(&column.name);
        let field = field.ok_or_else(|| invalid(COLUMNS))?;
        if tag_for(field.sql_type).is_none() {
            return Err(invalid("cursor ordering cannot use a JSON column"));
        }
        out.push(Term {
            field,
            direction: order.direction,
        });
    }
    let pk = M::META
        .pk()
        .ok_or_else(|| invalid("cursor pagination needs a primary key"))?;
    if !out.iter().any(|t| t.field.column == pk.column) {
        out.push(Term {
            field: pk,
            direction: OrderDirection::Asc,
        });
    }
    Ok(out)
}

/// A 64-bit hash of what identifies the list: the table, the filter with
/// its bound values and the ordering. `limit`, `offset`, the seek
/// predicate and related-object loading are not part of it.
///
/// The filter is hashed through its `Debug` form, so an upgrade of the
/// library can change fingerprints; old tokens are then rejected as
/// another query's.
pub(super) fn fingerprint(plan: &QueryPlan, terms: &[Term]) -> u64 {
    let mut hash = Sha256::new();
    let mut part = |text: &str| {
        hash.update(text.len().to_be_bytes());
        hash.update(text.as_bytes());
    };
    part(&plan.source.name);
    part(&format!("{:?}", plan.filter));
    for term in terms {
        part(term.field.column);
        part(&format!("{:?}", term.direction));
    }
    let digest = hash.finalize();
    digest
        .first_chunk::<8>()
        .map_or(0, |head| u64::from_be_bytes(*head))
}

/// `value` as the canonical [`Value`] of a column of type `ty`, whatever
/// form the backend returned it in.
pub(super) fn canonical(col: &str, ty: SqlType, value: Value) -> Built<Value> {
    if value.is_null() {
        return Ok(value);
    }
    convert(ty, value).map_err(|reason| QueryError::Decode {
        column: col.to_owned(),
        reason,
    })
}

fn int(value: Value) -> Result<Value, String> {
    i64::from_value(value).map(Value::Int)
}

fn convert(ty: SqlType, value: Value) -> Result<Value, String> {
    use SqlType as T;
    Ok(match ty {
        T::SmallInt | T::Integer | T::BigInt | T::Duration => int(value)?,
        T::Real | T::Double => Value::Float(f64::from_value(value)?),
        T::Decimal => Value::Decimal(Decimal::from_value(value)?),
        T::Bool => Value::Bool(bool::from_value(value)?),
        T::Text | T::IpAddr => Value::Text(String::from_value(value)?),
        T::Binary => Value::Bytes(Vec::from_value(value)?),
        T::Date => Value::Date(NaiveDate::from_value(value)?),
        T::Time => Value::Time(NaiveTime::from_value(value)?),
        T::Timestamp => Value::Timestamp(<DateTime<Utc>>::from_value(value)?),
        T::Uuid => Value::Uuid(Uuid::from_value(value)?),
        T::Json => return Err("JSON values have no position".to_owned()),
    })
}

/// How a page refers to the terms: their direction (inverted when paging
/// backwards) and, when the plan has joins, the root source that qualifies
/// column names so one shared with a joined table is not ambiguous.
pub(super) struct Seek<'a> {
    terms: &'a [Term],
    backward: bool,
    qualifier: Option<Ident>,
}

impl<'a> Seek<'a> {
    pub(super) fn new(plan: &QueryPlan, terms: &'a [Term]) -> Self {
        let root = plan.source.reference().clone();
        Self {
            terms,
            backward: false,
            qualifier: (!plan.joins.is_empty()).then_some(root),
        }
    }

    /// Page backwards: every direction is inverted.
    pub(super) fn backwards(mut self, yes: bool) -> Self {
        self.backward = yes;
        self
    }

    fn column(&self, term: &Term) -> Expr {
        let name = term.field.column;
        Expr::Column(match &self.qualifier {
            Some(source) => Column::qualified(source.clone(), name),
            None => Column::new(name),
        })
    }

    /// The `ORDER BY` of a page. A nullable term is preceded by an
    /// `IS NULL` sort key in the same direction, which places nulls last
    /// when ascending and first when descending on every backend.
    pub(super) fn ordering(&self) -> Vec<OrderExpr> {
        let mut out = Vec::with_capacity(self.terms.len() * 2);
        for term in self.terms {
            let direction = term.way(self.backward);
            let expr = self.column(term);
            if term.field.nullable {
                let key = expr.clone().is_null(true);
                out.push(OrderExpr {
                    expr: key,
                    direction,
                });
            }
            out.push(OrderExpr { expr, direction });
        }
        out
    }

    /// The term is equal to the cursor's value.
    fn equal(&self, term: &Term, value: &Value) -> Expr {
        let expr = self.column(term);
        if value.is_null() {
            expr.is_null(true)
        } else {
            expr.eq(Expr::val(value.clone()))
        }
    }

    /// The term is past the cursor's value in the page's direction, with
    /// null larger than every value. `None` when nothing can follow.
    fn past(&self, term: &Term, value: &Value) -> Option<Expr> {
        let expr = self.column(term);
        let ascending = term.way(self.backward) == Asc;
        match (ascending, value.is_null()) {
            (true, true) => None,
            (false, true) => Some(expr.is_null(false)),
            (true, false) => {
                let after = expr.clone().gt(Expr::val(value.clone()));
                Some(if term.field.nullable {
                    after.or(expr.is_null(true))
                } else {
                    after
                })
            }
            (false, false) => Some(expr.lt(Expr::val(value.clone()))),
        }
    }

    /// The expanded seek predicate: for some `k`, the first `k - 1` terms
    /// equal the cursor's values and term `k` is past its value.
    pub(super) fn predicate(&self, values: &[Value]) -> Expr {
        let mut clauses = Vec::new();
        let pairs = self.terms.iter().zip(values);
        for (k, (term, value)) in pairs.clone().enumerate() {
            let Some(last) = self.past(term, value) else {
                continue;
            };
            let mut parts: Vec<Expr> = pairs
                .clone()
                .take(k)
                .map(|(t, v)| self.equal(t, v))
                .collect();
            if parts.is_empty() {
                clauses.push(last);
            } else {
                parts.push(last);
                clauses.push(Expr::And(parts));
            }
        }
        Expr::Or(clauses)
    }
}
