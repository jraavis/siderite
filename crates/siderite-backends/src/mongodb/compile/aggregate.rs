//! Aggregate functions and their `$group` accumulators.

use super::expr::{expr_bson, isnull, let_in};
use super::{Ctx, Res, Scope, invalid, unsupported};
use ::mongodb::bson::{Bson, doc};
use siderite_orm::expr::{AggFunc, Aggregate};
use siderite_orm::{Expr, Feature};

pub(super) struct AggPlan {
    /// `$group` fields for this aggregate.
    pub(super) accumulators: Vec<(String, Bson)>,
    /// Expression over the group's fields producing the SQL result.
    pub(super) finish: Bson,
}

fn acc(name: &str, op: &str, arg: Bson) -> (String, Bson) {
    (name.to_owned(), Bson::Document(doc! { op: arg }))
}

fn non_null_items(array: &str) -> Bson {
    Bson::Document(doc! { "$filter": {
        "input": array,
        "cond": { "$ne": ["$$this", Bson::Null] },
    } })
}

#[allow(clippy::too_many_lines)]
pub(super) fn aggregate_plan(i: usize, agg: &Aggregate, ctx: &Ctx<'_>) -> Res<AggPlan> {
    let main = format!("a{i}");
    let num = format!("a{i}n");
    let field = format!("${main}");
    let row = Scope::row(ctx);
    let raw = match &agg.arg {
        Some(arg) => Some(expr_bson(arg, &row)?),
        None => None,
    };
    let filter = match &agg.filter {
        Some(f) => Some(expr_bson(f, &row)?),
        None => None,
    };
    // Rows failing FILTER contribute NULL, which every accumulator ignores.
    let arg = raw.map(|a| match &filter {
        Some(f) => Bson::Document(doc! { "$cond": [f.clone(), a, Bson::Null] }),
        None => a,
    });
    let func = &agg.func;
    let Some(arg) = arg else {
        return match func {
            AggFunc::Count if !agg.distinct => {
                let one = match &filter {
                    Some(f) => Bson::Document(doc! { "$cond": [f.clone(), 1_i64, 0_i64] }),
                    None => Bson::Int64(1),
                };
                Ok(AggPlan {
                    accumulators: vec![acc(&main, "$sum", one)],
                    finish: Bson::String(field),
                })
            }
            _ => Err(invalid("only COUNT(*) may omit its argument")),
        };
    };
    let distinct_items = || {
        (
            main.clone(),
            Bson::Document(doc! { "$addToSet": arg.clone() }),
        )
    };
    let items = non_null_items(&field);
    let over_items = |op: &str| Bson::Document(doc! { op: items.clone() });
    // Empty input to an aggregate that SQL defines as NULL.
    let nonempty = |value: Bson| {
        Bson::Document(doc! { "$cond": [
            { "$eq": [ { "$size": items.clone() }, 0_i64 ] },
            Bson::Null,
            value,
        ] })
    };
    Ok(match func {
        AggFunc::Count if agg.distinct => AggPlan {
            accumulators: vec![distinct_items()],
            finish: Bson::Document(doc! { "$size": items }),
        },
        AggFunc::Count => AggPlan {
            accumulators: vec![acc(
                &main,
                "$sum",
                Bson::Document(doc! { "$cond": [isnull(arg), 0_i64, 1_i64] }),
            )],
            finish: Bson::String(field),
        },
        AggFunc::Sum if agg.distinct => AggPlan {
            accumulators: vec![distinct_items()],
            finish: nonempty(over_items("$sum")),
        },
        AggFunc::Sum => AggPlan {
            accumulators: vec![
                acc(&main, "$sum", arg.clone()),
                acc(
                    &num,
                    "$sum",
                    Bson::Document(doc! { "$cond": [ { "$isNumber": arg }, 1_i64, 0_i64 ] }),
                ),
            ],
            finish: Bson::Document(doc! { "$cond": [
                { "$eq": [format!("${num}"), 0_i64] },
                Bson::Null,
                field,
            ] }),
        },
        AggFunc::Avg if agg.distinct => AggPlan {
            accumulators: vec![distinct_items()],
            finish: over_items("$avg"),
        },
        AggFunc::Avg => AggPlan {
            accumulators: vec![acc(&main, "$avg", arg)],
            finish: Bson::String(field),
        },
        AggFunc::Min => AggPlan {
            accumulators: vec![acc(&main, "$min", arg)],
            finish: Bson::String(field),
        },
        AggFunc::Max => AggPlan {
            accumulators: vec![acc(&main, "$max", arg)],
            finish: Bson::String(field),
        },
        AggFunc::StdDev { sample } | AggFunc::Variance { sample } => {
            let op = if *sample { "$stdDevSamp" } else { "$stdDevPop" };
            let (accumulators, deviation) = if agg.distinct {
                (vec![distinct_items()], over_items(op))
            } else {
                (vec![acc(&main, op, arg)], Bson::String(field.clone()))
            };
            let finish = if matches!(func, AggFunc::Variance { .. }) {
                Bson::Document(doc! { "$pow": [deviation, 2_i64] })
            } else {
                deviation
            };
            AggPlan {
                accumulators,
                finish,
            }
        }
        AggFunc::StringAgg { separator } => {
            let collect = if agg.distinct {
                distinct_items()
            } else {
                (main.clone(), Bson::Document(doc! { "$push": arg }))
            };
            let joined = let_in(
                "arr",
                items,
                Bson::Document(doc! { "$cond": [
                    { "$eq": [ { "$size": "$$arr" }, 0_i64 ] },
                    Bson::Null,
                    { "$reduce": {
                        "input": { "$slice": ["$$arr", 1_i64, { "$size": "$$arr" }] },
                        "initialValue": { "$arrayElemAt": ["$$arr", 0_i64] },
                        "in": { "$concat": ["$$value", separator.as_str(), "$$this"] },
                    } },
                ] }),
            );
            AggPlan {
                accumulators: vec![collect],
                finish: joined,
            }
        }
        AggFunc::ArrayAgg => return Err(unsupported(Feature::Arrays)),
    })
}

/// Collect every distinct aggregate of `expr` (not descending into their
/// arguments), in first-seen order.
pub(super) fn collect_aggregates(expr: &Expr, out: &mut Vec<Aggregate>) {
    if let Expr::Aggregate(agg) = expr {
        if !out.contains(agg) {
            out.push(agg.clone());
        }
        return;
    }
    for child in expr.children() {
        collect_aggregates(child, out);
    }
}
