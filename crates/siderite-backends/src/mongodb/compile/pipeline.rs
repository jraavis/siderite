//! Pipeline assembly: stages, projection, sorting and pagination.

use super::aggregate::{aggregate_plan, collect_aggregates};
use super::expr::expr_bson;
use super::filter::filter_doc;
use super::{Ctx, GroupScope, Keys, Res, Scope};
use super::{check_name, invalid, unsupported};
use ::mongodb::bson::{Bson, Document, doc};
use siderite_orm::expr::AggFunc;
use siderite_orm::{DistinctMode, Expr, Feature, OrderDirection};
use siderite_orm::{QueryPlan, SelectExpr, Value};

pub(super) struct Built {
    pub(super) collection: String,
    pub(super) pipeline: Vec<Document>,
    pub(super) columns: Vec<String>,
    pub(super) pk: String,
    pub(super) empty_row: Option<Vec<Value>>,
}

fn output_name(select: &SelectExpr, index: usize) -> String {
    match (&select.alias, &select.expr) {
        (Some(alias), _) => alias.to_string(),
        (None, Expr::Column(c)) => c.name.to_string(),
        (None, _) => format!("col{index}"),
    }
}

fn to_i64(n: u64, what: &str) -> Res<i64> {
    i64::try_from(n).map_err(|_| invalid(format!("{what} {n} is too large")))
}

#[allow(clippy::too_many_lines)]
pub(super) fn build(plan: &QueryPlan, keys: &Keys) -> Res<Built> {
    // Rejections in case the caller skipped `check`.
    if !plan.joins.is_empty() {
        return Err(unsupported(Feature::Joins));
    }
    if plan.lock.is_some() {
        return Err(unsupported(Feature::RowLocking));
    }
    if !plan.compound.is_empty() {
        return Err(unsupported(Feature::SetOperations));
    }
    if matches!(plan.distinct, DistinctMode::On(_)) {
        return Err(unsupported(Feature::DistinctOn));
    }

    let mut pipeline: Vec<Document> = Vec::new();
    let (collection, root_pk, ctx_pk) = match &plan.source.subquery {
        Some(inner) => {
            let inner = build(inner, keys)?;
            pipeline = inner.pipeline;
            if inner.columns.is_empty() {
                // Expose the key under its column name.
                pipeline.push(doc! { "$addFields": { inner.pk.as_str(): "$_id" } });
                pipeline.push(doc! { "$unset": "_id" });
            }
            (inner.collection, inner.pk, None)
        }
        None => {
            check_name(&plan.source.name)?;
            let pk = keys.pk(&plan.source.name).to_owned();
            (plan.source.name.to_string(), pk.clone(), Some(pk))
        }
    };
    let ctx = Ctx {
        pk: ctx_pk.as_deref(),
        root: plan.source.reference().as_ref(),
    };

    if let Some(filter) = &plan.filter {
        let doc = filter_doc(filter, false, &ctx)?;
        if !doc.is_empty() {
            pipeline.push(doc! { "$match": doc });
        }
    }

    let has_aggregate = plan
        .projection
        .iter()
        .map(|s| &s.expr)
        .chain(plan.having.iter())
        .chain(plan.ordering.iter().map(|o| &o.expr))
        .any(Expr::contains_aggregate);
    let grouped = has_aggregate || !plan.grouping.is_empty() || plan.having.is_some();

    let columns: Vec<String> = plan
        .projection
        .iter()
        .enumerate()
        .map(|(i, s)| output_name(s, i))
        .collect();
    let distinct = plan.distinct == DistinctMode::All && !columns.is_empty();
    let mut empty_row = None;

    if grouped {
        if plan.projection.is_empty() {
            return Err(invalid("a grouped query needs an explicit projection"));
        }
        let mut aggs = Vec::new();
        plan.projection
            .iter()
            .map(|s| &s.expr)
            .chain(plan.having.iter())
            .chain(plan.ordering.iter().map(|o| &o.expr))
            .for_each(|e| collect_aggregates(e, &mut aggs));
        let group = GroupScope {
            keys: plan.grouping.clone(),
            aggs,
            aliases: plan
                .projection
                .iter()
                .zip(&columns)
                .map(|(s, name)| (name.clone(), s.expr.clone()))
                .collect(),
        };
        let row_scope = Scope::row(&ctx);
        let key = if group.keys.is_empty() {
            Bson::Null
        } else {
            let mut key = Document::new();
            for (i, k) in group.keys.iter().enumerate() {
                key.insert(format!("k{i}"), expr_bson(k, &row_scope)?);
            }
            Bson::Document(key)
        };
        let mut group_doc = doc! { "_id": key };
        for (i, agg) in group.aggs.iter().enumerate() {
            for (name, spec) in aggregate_plan(i, agg, &ctx)?.accumulators {
                group_doc.insert(name, spec);
            }
        }

        let only_count_star = group.keys.is_empty()
            && plan.having.is_none()
            && plan.projection.len() == 1
            && matches!(&plan.projection[0].expr, Expr::Aggregate(a)
                if a.func == AggFunc::Count && a.arg.is_none() && a.filter.is_none() && !a.distinct);
        if only_count_star {
            pipeline.push(doc! { "$count": columns[0].as_str() });
        } else {
            pipeline.push(doc! { "$group": group_doc });
        }

        let post = Scope {
            ctx: &ctx,
            group: Some(&group),
        };
        if !only_count_star && let Some(having) = &plan.having {
            pipeline.push(doc! { "$match": { "$expr": expr_bson(having, &post)? } });
        }
        let mut project = doc! { "_id": 0_i64 };
        for (select, name) in plan.projection.iter().zip(&columns) {
            let value = if only_count_star {
                Bson::String(format!("${name}"))
            } else {
                expr_bson(&select.expr, &post)?
            };
            project.insert(name.clone(), value);
        }
        let mut temps = Vec::new();
        let sort = sort_document(plan, &columns, |i, expr| {
            let name = format!("__sort{i}");
            project.insert(name.clone(), expr_bson(expr, &post)?);
            temps.push(name.clone());
            Ok(name)
        })?;
        if !only_count_star {
            pipeline.push(doc! { "$project": project });
        }
        if group.keys.is_empty()
            && plan.having.is_none()
            && plan.offset.unwrap_or(0) == 0
            && plan.limit != Some(0)
        {
            empty_row = Some(
                plan.projection
                    .iter()
                    .map(|s| match &s.expr {
                        Expr::Aggregate(a) if a.func == AggFunc::Count => Value::Int(0),
                        _ => Value::Null,
                    })
                    .collect(),
            );
        }
        finish_pipeline(&mut pipeline, plan, distinct, &columns, sort, &temps)?;
    } else if distinct {
        // DISTINCT: de-duplicate the projected rows, then order them.
        let scope = Scope::row(&ctx);
        pipeline.push(doc! { "$project": projection_doc(plan, &columns, &scope)? });
        let sort = sort_document(plan, &columns, |_, _| {
            Err(invalid(
                "with DISTINCT, ORDER BY must use projected expressions",
            ))
        })?;
        finish_pipeline(&mut pipeline, plan, true, &columns, sort, &[])?;
    } else {
        let scope = Scope::row(&ctx);
        let mut temps = Vec::new();
        let mut add_fields = Document::new();
        let sort = sort_document_source(plan, &scope, &mut add_fields, &mut temps)?;
        if !add_fields.is_empty() {
            pipeline.push(doc! { "$addFields": add_fields });
        }
        if let Some(sort) = sort {
            pipeline.push(doc! { "$sort": sort });
        }
        if let Some(n) = plan.offset
            && n > 0
        {
            pipeline.push(doc! { "$skip": to_i64(n, "offset")? });
        }
        if let Some(n) = plan.limit {
            pipeline.push(doc! { "$limit": to_i64(n, "limit")? });
        }
        if columns.is_empty() {
            if !temps.is_empty() {
                pipeline.push(doc! { "$unset": temps });
            }
        } else {
            pipeline.push(doc! { "$project": projection_doc(plan, &columns, &scope)? });
        }
    }

    Ok(Built {
        collection,
        pipeline,
        columns,
        pk: root_pk,
        empty_row,
    })
}

fn projection_doc(plan: &QueryPlan, columns: &[String], scope: &Scope<'_>) -> Res<Document> {
    let mut project = doc! { "_id": 0_i64 };
    for (select, name) in plan.projection.iter().zip(columns) {
        check_name(name)?;
        project.insert(name.clone(), expr_bson(&select.expr, scope)?);
    }
    Ok(project)
}

/// Sort keys evaluated on source documents (ungrouped, non-distinct).
fn sort_document_source(
    plan: &QueryPlan,
    scope: &Scope<'_>,
    add_fields: &mut Document,
    temps: &mut Vec<String>,
) -> Res<Option<Document>> {
    if plan.ordering.is_empty() {
        return Ok(None);
    }
    let mut sort = Document::new();
    for (i, order) in plan.ordering.iter().enumerate() {
        let direction = match order.direction {
            OrderDirection::Asc => 1_i32,
            OrderDirection::Desc => -1_i32,
        };
        let name = match &order.expr {
            Expr::Column(c) => scope.ctx.field(c)?,
            other => {
                let name = format!("__sort{i}");
                add_fields.insert(name.clone(), expr_bson(other, scope)?);
                temps.push(name.clone());
                name
            }
        };
        sort.insert(name, direction);
    }
    Ok(Some(sort))
}

/// Sort keys over projected output (grouped or distinct): an ordering that
/// equals a projected expression or names an output column uses that
/// output; anything else is handed to `extra`.
fn sort_document(
    plan: &QueryPlan,
    columns: &[String],
    mut extra: impl FnMut(usize, &Expr) -> Res<String>,
) -> Res<Option<Document>> {
    if plan.ordering.is_empty() {
        return Ok(None);
    }
    let mut sort = Document::new();
    for (i, order) in plan.ordering.iter().enumerate() {
        let direction = match order.direction {
            OrderDirection::Asc => 1_i32,
            OrderDirection::Desc => -1_i32,
        };
        let by_expr = plan
            .projection
            .iter()
            .position(|s| s.expr == order.expr)
            .map(|p| columns[p].clone());
        let by_name = match &order.expr {
            Expr::Column(c) if c.source.is_none() => columns
                .iter()
                .find(|name| **name == c.name.as_ref())
                .cloned(),
            _ => None,
        };
        let name = match by_expr.or(by_name) {
            Some(name) => name,
            None => extra(i, &order.expr)?,
        };
        sort.insert(name, direction);
    }
    Ok(Some(sort))
}

fn finish_pipeline(
    pipeline: &mut Vec<Document>,
    plan: &QueryPlan,
    distinct: bool,
    columns: &[String],
    sort: Option<Document>,
    temps: &[String],
) -> Res<()> {
    if distinct {
        let mut key = Document::new();
        for name in columns {
            key.insert(name.clone(), format!("${name}"));
        }
        pipeline.push(doc! { "$group": { "_id": key } });
        pipeline.push(doc! { "$replaceRoot": { "newRoot": "$_id" } });
    }
    if let Some(sort) = sort {
        pipeline.push(doc! { "$sort": sort });
    }
    if let Some(n) = plan.offset
        && n > 0
    {
        pipeline.push(doc! { "$skip": to_i64(n, "offset")? });
    }
    if let Some(n) = plan.limit {
        pipeline.push(doc! { "$limit": to_i64(n, "limit")? });
    }
    if !temps.is_empty() {
        pipeline.push(doc! { "$unset": temps.to_vec() });
    }
    Ok(())
}
