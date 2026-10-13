//! Terminal methods that read models.

use super::QuerySet;
use super::related::attach_all;
use crate::backend::{QueryResult, Row};
use crate::error::{OrmError, QueryError};
use crate::expr::Expr;
use crate::model::Model;
use crate::persist::{pk_field, pk_filter};
use crate::plan::{DistinctMode, QueryPlan, SelectExpr};
use crate::types::DbType;
use std::collections::HashMap;
use std::hash::Hash;

/// Models decoded from fetched rows, each with its row.
pub(super) type Decoded<M> = Result<Vec<(M, Row)>, OrmError>;

/// One page of a [`QuerySet::paginate`] result.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<M> {
    /// Rows of this page.
    pub items: Vec<M>,
    /// Rows matching the queryset over all pages.
    pub total: u64,
    /// 1-based page number.
    pub page: u64,
    /// Requested page size.
    pub per_page: u64,
}

impl<M> Page<M> {
    /// Number of pages needed for [`total`](Self::total) rows.
    pub fn total_pages(&self) -> u64 {
        self.total.div_ceil(self.per_page.max(1))
    }
}

/// Whether `plan` can be counted / tested for existence with a plain
/// aggregate instead of a derived table.
pub(super) fn is_flat(plan: &QueryPlan) -> bool {
    plan.grouping.is_empty()
        && plan.having.is_none()
        && plan.distinct == DistinctMode::None
        && plan.limit.is_none()
        && plan.offset.is_none()
        && plan.compound.is_empty()
}

/// `plan` without row locks and, unless a limit makes it meaningful, without
/// ordering (PostgreSQL rejects `ORDER BY` next to aggregates).
pub(super) fn unordered(plan: &QueryPlan) -> QueryPlan {
    let mut plan = plan.clone();
    plan.lock = None;
    if plan.limit.is_none() && plan.offset.is_none() {
        plan.ordering.clear();
    }
    plan
}

/// The first column of the first row as an unsigned count.
pub(super) fn scalar_count(result: &QueryResult) -> Result<u64, QueryError> {
    let row = result.rows.first().ok_or_else(|| QueryError::Decode {
        column: "count".into(),
        reason: "no row returned".into(),
    })?;
    row.decode_at::<i64>(0)
        .map(|n| u64::try_from(n).unwrap_or(0))
}

impl<M: Model> QuerySet<M> {
    /// Run the query and decode every row.
    ///
    /// # Errors
    /// Capability, backend or decode errors.
    pub async fn all(self) -> Result<Vec<M>, OrmError> {
        Ok(self
            .all_annotated()
            .await?
            .into_iter()
            .map(|(model, _)| model)
            .collect())
    }

    /// Like [`all`](Self::all), pairing each model with its full result row,
    /// so `annotate`d columns can be read with [`Row::get_as`].
    ///
    /// # Errors
    /// As [`all`](Self::all).
    pub async fn all_annotated(self) -> Result<Vec<(M, Row)>, OrmError> {
        self.ready()?;
        if self.empty {
            return Ok(Vec::new());
        }
        let result = self.db.fetch(&self.plan).await?;
        self.decode_rows(result.rows).await
    }

    /// Decode fetched `rows` into models, attaching `select_related`
    /// objects and running the prefetches.
    pub(super) async fn decode_rows(&self, rows: Vec<Row>) -> Decoded<M> {
        let mut models = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut model = M::from_row(row, "")?;
            attach_all(&mut model, &self.related, row)?;
            models.push(model);
        }
        self.run_prefetches(&mut models).await?;
        Ok(models.into_iter().zip(rows).collect())
    }

    /// The first row, if any. An unordered queryset is ordered by primary key.
    ///
    /// # Errors
    /// As [`all`](Self::all).
    pub async fn first(self) -> Result<Option<M>, OrmError> {
        let ordered = if self.plan.ordering.is_empty() {
            let pk = pk_field::<M>()?.column;
            self.with_plan(|p| p.order_by(Expr::col(pk), crate::OrderDirection::Asc))
        } else {
            self
        };
        Ok(ordered.limit(1).all().await?.into_iter().next())
    }

    /// The last row, if any: [`first`](Self::first) on the reversed ordering.
    ///
    /// # Errors
    /// As [`all`](Self::all).
    pub async fn last(self) -> Result<Option<M>, OrmError> {
        self.reverse().first().await
    }

    /// The row with the smallest `field`.
    ///
    /// # Errors
    /// [`QueryError::DoesNotExist`] on an empty queryset, or as [`all`](Self::all).
    pub async fn earliest(self, field: impl Into<Expr>) -> Result<M, OrmError> {
        self.order_by([field.into().asc()]).first_or_missing().await
    }

    /// The row with the largest `field`.
    ///
    /// # Errors
    /// [`QueryError::DoesNotExist`] on an empty queryset, or as [`all`](Self::all).
    pub async fn latest(self, field: impl Into<Expr>) -> Result<M, OrmError> {
        self.order_by([field.into().desc()])
            .first_or_missing()
            .await
    }

    async fn first_or_missing(self) -> Result<M, OrmError> {
        self.first()
            .await?
            .ok_or_else(|| QueryError::DoesNotExist.into())
    }

    /// Exactly one row matching `predicate`.
    ///
    /// # Errors
    /// [`QueryError::DoesNotExist`], [`QueryError::MultipleObjectsReturned`],
    /// or as [`all`](Self::all).
    pub async fn get(self, predicate: Expr) -> Result<M, OrmError> {
        self.filter(predicate).one().await
    }

    /// The only row of this queryset.
    pub(super) async fn one(self) -> Result<M, OrmError> {
        let mut rows = self.limit(2).all().await?;
        match rows.len() {
            0 => Err(QueryError::DoesNotExist.into()),
            1 => rows.pop().ok_or_else(|| QueryError::DoesNotExist.into()),
            n => Err(QueryError::MultipleObjectsReturned(n as u64).into()),
        }
    }

    /// Number of matching rows.
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn count(self) -> Result<u64, OrmError> {
        self.ready()?;
        if self.empty {
            return Ok(0);
        }
        let count = Expr::from(crate::Count::all());
        let plan = if is_flat(&self.plan) {
            let mut plan = unordered(&self.plan);
            plan.projection = vec![SelectExpr::new(count, Some("count".into()))];
            plan
        } else {
            QueryPlan::from_subquery(unordered(&self.plan), "counted").select(count, Some("count"))
        };
        Ok(scalar_count(&self.db.fetch(&plan).await?)?)
    }

    /// Whether at least one row matches.
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn exists(self) -> Result<bool, OrmError> {
        self.ready()?;
        if self.empty {
            return Ok(false);
        }
        if !is_flat(&self.plan) {
            return Ok(self.count().await? > 0);
        }
        let mut plan = unordered(&self.plan);
        plan.projection = vec![SelectExpr::new(Expr::val(1), Some("one".into()))];
        plan.limit = Some(1);
        Ok(!self.db.fetch(&plan).await?.rows.is_empty())
    }

    /// Whether `object` is among the matching rows (by primary key).
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn contains(self, object: &M) -> Result<bool, OrmError> {
        let predicate = pk_filter::<M>(&object.pk())?;
        self.filter(predicate).exists().await
    }

    /// One page of the queryset (`page` starts at 1) plus the total count.
    ///
    /// The plan is checked against the backend before the count runs.
    ///
    /// # Errors
    /// [`QueryError::InvalidPlan`] for `page == 0` or `per_page == 0`;
    /// capability or backend errors.
    pub async fn paginate(self, page: u64, per_page: u64) -> Result<Page<M>, OrmError> {
        if page == 0 || per_page == 0 {
            return Err(QueryError::InvalidPlan("page and per_page start at 1".into()).into());
        }
        self.ready()?;
        self.plan.check(&self.db.capabilities())?;
        let total = self.clone().count().await?;
        let items = self
            .offset((page - 1).saturating_mul(per_page))
            .limit(per_page)
            .all()
            .await?;
        Ok(Page {
            items,
            total,
            page,
            per_page,
        })
    }

    /// Rows whose primary key is in `ids`, keyed by primary key
    /// (Django `in_bulk`). Missing keys are absent from the map.
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn in_bulk(
        self,
        ids: impl IntoIterator<Item = M::Pk>,
    ) -> Result<HashMap<M::Pk, M>, OrmError>
    where
        M::Pk: Eq + Hash,
    {
        let ids: Vec<M::Pk> = ids.into_iter().collect();
        let column = pk_field::<M>()?.column;
        let chunk = self.db.capabilities().max_params.max(1);
        let mut found = HashMap::with_capacity(ids.len());
        for ids in ids.chunks(chunk) {
            let predicate = Expr::col(column).is_in(ids.iter().map(DbType::to_value));
            for model in self.clone().filter(predicate).all().await? {
                found.insert(model.pk(), model);
            }
        }
        Ok(found)
    }
}
