//! Aggregation-expression compilation (`$expr` operands, functions, casts).

use super::super::value::to_bson;
use super::aggregate::aggregate_plan;
use super::filter::{check_pattern, escape_regex};
use super::{Res, Scope, invalid, unsupported};
use ::mongodb::bson::{Bson, doc};
use siderite_orm::expr::{BinaryOp, DatePart, Function, Lookup, UnaryOp};
use siderite_orm::{Expr, Feature, SqlType, Value};

pub(super) fn isnull(x: Bson) -> Bson {
    let eq = doc! { "$eq": [ { "$ifNull": [x, Bson::Null] }, Bson::Null ] };
    Bson::Document(eq)
}

fn literal(x: Bson) -> Bson {
    Bson::Document(doc! { "$literal": x })
}

/// `x` unless it is NULL, then NULL (`inner` computes the non-NULL case).
fn null_guard(x: &Bson, inner: Bson) -> Bson {
    Bson::Document(doc! { "$cond": [isnull(x.clone()), Bson::Null, inner] })
}

pub(super) fn let_in(var: &str, value: Bson, body: Bson) -> Bson {
    Bson::Document(doc! { "$let": { "vars": { var: value }, "in": body } })
}

fn is_nonnull_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::Value(v) if !v.is_null())
}

pub(super) fn expr_bson(expr: &Expr, scope: &Scope<'_>) -> Res<Bson> {
    if let Some(group) = scope.group {
        if let Some(i) = group.keys.iter().position(|k| k == expr) {
            return Ok(Bson::String(format!("$_id.k{i}")));
        }
        if let Expr::Aggregate(agg) = expr {
            let i = group
                .aggs
                .iter()
                .position(|a| a == agg)
                .ok_or_else(|| invalid("aggregate was not collected"))?;
            return Ok(aggregate_plan(i, agg, scope.ctx)?.finish);
        }
        if let Expr::Column(c) = expr {
            if c.source.is_none()
                && let Some((_, e)) = group
                    .aliases
                    .iter()
                    .find(|(a, e)| *a == c.name.as_ref() && e != expr)
            {
                return expr_bson(e, scope);
            }
            return Err(invalid(format!(
                "column `{}` must appear in the grouping or inside an aggregate",
                c.name
            )));
        }
    }
    match expr {
        Expr::Column(c) => Ok(Bson::String(format!("${}", scope.ctx.field(c)?))),
        Expr::Value(Value::Null) => Ok(Bson::Null),
        Expr::Value(v) => Ok(literal(to_bson(v)?)),
        Expr::Binary { op, lhs, rhs } => binary(*op, lhs, rhs, scope),
        Expr::Unary { op, expr } => {
            let x = expr_bson(expr, scope)?;
            Ok(match op {
                UnaryOp::Neg => Bson::Document(doc! { "$multiply": [x, -1_i64] }),
                UnaryOp::Not => let_in(
                    "v",
                    x,
                    Bson::Document(doc! { "$cond": [
                        isnull(Bson::String("$$v".into())),
                        Bson::Null,
                        { "$not": ["$$v"] },
                    ] }),
                ),
            })
        }
        Expr::And(items) => kleene(items, scope, false),
        Expr::Or(items) => kleene(items, scope, true),
        Expr::Lookup { expr, lookup } => lookup_bson(expr, lookup, scope),
        Expr::Func { func, args } => function(*func, args, scope),
        Expr::Cast { expr, ty } => cast(expr, *ty, scope),
        Expr::Case {
            branches,
            otherwise,
        } => {
            let mut cases = Vec::with_capacity(branches.len());
            for (when, then) in branches {
                cases.push(Bson::Document(doc! {
                    "case": expr_bson(when, scope)?,
                    "then": expr_bson(then, scope)?,
                }));
            }
            let default = match otherwise {
                Some(e) => expr_bson(e, scope)?,
                None => Bson::Null,
            };
            Ok(Bson::Document(
                doc! { "$switch": { "branches": cases, "default": default } },
            ))
        }
        Expr::DatePart { part, expr } => date_part(*part, &expr_bson(expr, scope)?),
        Expr::Aggregate(_) => Err(invalid(
            "aggregates are only allowed in the projection, HAVING or ordering of a grouped query",
        )),
        Expr::Window(_) => Err(unsupported(Feature::WindowFunctions)),
        Expr::Exists(_) | Expr::Subquery(_) => Err(unsupported(Feature::Subqueries)),
        Expr::Related(_) => Err(unsupported(Feature::Joins)),
        Expr::OuterRef(_) => Err(invalid("OuterRef is only valid inside a subquery")),
    }
}

fn binary(op: BinaryOp, lhs: &Expr, rhs: &Expr, scope: &Scope<'_>) -> Res<Bson> {
    let a = expr_bson(lhs, scope)?;
    let b = expr_bson(rhs, scope)?;
    let arithmetic = |key: &str| Bson::Document(doc! { key: [a.clone(), b.clone()] });
    let key = match op {
        BinaryOp::Add => return Ok(arithmetic("$add")),
        BinaryOp::Sub => return Ok(arithmetic("$subtract")),
        BinaryOp::Mul => return Ok(arithmetic("$multiply")),
        BinaryOp::Div => return Ok(arithmetic("$divide")),
        BinaryOp::Mod => return Ok(arithmetic("$mod")),
        BinaryOp::Eq => "$eq",
        BinaryOp::Ne => "$ne",
        BinaryOp::Lt => "$lt",
        BinaryOp::Le => "$lte",
        BinaryOp::Gt => "$gt",
        BinaryOp::Ge => "$gte",
    };
    // Comparison with the NULL literal follows the plan docs: `= NULL` is
    // `IS NULL`.
    let lnull = matches!(lhs, Expr::Value(Value::Null));
    let rnull = matches!(rhs, Expr::Value(Value::Null));
    if lnull || rnull {
        let other = if lnull { b } else { a };
        return Ok(match op {
            BinaryOp::Eq => isnull(other),
            BinaryOp::Ne => Bson::Document(doc! { "$not": [isnull(other)] }),
            _ => Bson::Null,
        });
    }
    let mut guards = Vec::new();
    if !is_nonnull_literal(lhs) {
        guards.push(isnull(a.clone()));
    }
    if !is_nonnull_literal(rhs) {
        guards.push(isnull(b.clone()));
    }
    let cmp = Bson::Document(doc! { key: [a, b] });
    Ok(match guards.len() {
        0 => cmp,
        1 => Bson::Document(doc! { "$cond": [guards.remove(0), Bson::Null, cmp] }),
        _ => Bson::Document(doc! { "$cond": [ { "$or": guards }, Bson::Null, cmp ] }),
    })
}

/// Three-valued AND / OR: NULL when undecided.
fn kleene(items: &[Expr], scope: &Scope<'_>, or: bool) -> Res<Bson> {
    let mut parts = Vec::with_capacity(items.len());
    for item in items {
        parts.push(expr_bson(item, scope)?);
    }
    match parts.len() {
        0 => return Ok(Bson::Boolean(!or)),
        1 => return Ok(parts.remove(0)),
        _ => {}
    }
    let (decisive, otherwise) = (or, !or);
    let v = || Bson::String("$$v".into());
    Ok(let_in(
        "v",
        Bson::Array(parts),
        Bson::Document(doc! { "$cond": [
            { "$in": [decisive, v()] },
            decisive,
            { "$cond": [ { "$in": [Bson::Null, v()] }, Bson::Null, otherwise ] },
        ] }),
    ))
}

fn text_pattern(lookup: &Lookup) -> Res<Option<(String, bool)>> {
    Ok(match lookup {
        Lookup::IExact(Value::Text(t)) => Some((format!("^{}\\z", escape_regex(t)?), true)),
        Lookup::Contains {
            needle,
            case_insensitive,
        } => Some((escape_regex(needle)?, *case_insensitive)),
        Lookup::StartsWith {
            needle,
            case_insensitive,
        } => Some((format!("^{}", escape_regex(needle)?), *case_insensitive)),
        Lookup::EndsWith {
            needle,
            case_insensitive,
        } => Some((format!("{}\\z", escape_regex(needle)?), *case_insensitive)),
        Lookup::Regex(p) => {
            check_pattern(p)?;
            Some((p.clone(), false))
        }
        _ => None,
    })
}

fn lookup_bson(target: &Expr, lookup: &Lookup, scope: &Scope<'_>) -> Res<Bson> {
    let x = expr_bson(target, scope)?;
    if let Some((pattern, insensitive)) = text_pattern(lookup)? {
        let mut spec = doc! { "input": x.clone(), "regex": pattern };
        if insensitive {
            spec.insert("options", "i");
        }
        return Ok(Bson::Document(doc! { "$cond": [
            { "$eq": [ { "$type": x }, "string" ] },
            { "$regexMatch": spec },
            Bson::Null,
        ] }));
    }
    Ok(match lookup {
        Lookup::IsNull(yes) => {
            if *yes {
                isnull(x)
            } else {
                Bson::Document(doc! { "$not": [isnull(x)] })
            }
        }
        Lookup::IExact(Value::Null) => isnull(x),
        Lookup::IExact(v) => {
            let inner = Bson::Document(doc! { "$eq": [x.clone(), literal(to_bson(v)?)] });
            null_guard(&x, inner)
        }
        Lookup::In(values) => {
            let list: Vec<Bson> = values
                .iter()
                .filter(|v| !v.is_null())
                .map(to_bson)
                .collect::<Result<_, _>>()?;
            if list.is_empty() {
                Bson::Boolean(false)
            } else {
                let inner = Bson::Document(doc! { "$in": [x.clone(), literal(Bson::Array(list))] });
                null_guard(&x, inner)
            }
        }
        Lookup::Range(lo, hi) => {
            if lo.is_null() || hi.is_null() {
                Bson::Boolean(false)
            } else {
                let inner = Bson::Document(doc! { "$and": [
                    { "$gte": [x.clone(), literal(to_bson(lo)?)] },
                    { "$lte": [x.clone(), literal(to_bson(hi)?)] },
                ] });
                null_guard(&x, inner)
            }
        }
        Lookup::InSubquery(_) => return Err(unsupported(Feature::Subqueries)),
        // Text lookups returned above.
        _ => return Err(invalid("unsupported lookup")),
    })
}

fn arity(func: Function, args: &[Expr], wanted: std::ops::RangeInclusive<usize>) -> Res<()> {
    if wanted.contains(&args.len()) {
        Ok(())
    } else {
        Err(invalid(format!(
            "{func:?} takes {}..={} arguments, got {}",
            wanted.start(),
            wanted.end(),
            args.len()
        )))
    }
}

fn function(func: Function, args: &[Expr], scope: &Scope<'_>) -> Res<Bson> {
    let mut compiled = Vec::with_capacity(args.len());
    for arg in args {
        compiled.push(expr_bson(arg, scope)?);
    }
    Ok(match func {
        Function::Lower | Function::Upper | Function::Length | Function::Trim => {
            arity(func, args, 1..=1)?;
            let x = compiled.remove(0);
            let inner = match func {
                Function::Lower => doc! { "$toLower": x.clone() },
                Function::Upper => doc! { "$toUpper": x.clone() },
                Function::Length => doc! { "$strLenCP": x.clone() },
                _ => doc! { "$trim": { "input": x.clone(), "chars": " " } },
            };
            null_guard(&x, Bson::Document(inner))
        }
        Function::Coalesce => {
            arity(func, args, 1..=usize::MAX)?;
            if compiled.len() == 1 {
                compiled.remove(0)
            } else {
                Bson::Document(doc! { "$ifNull": compiled })
            }
        }
        Function::Concat => {
            let parts: Vec<Bson> = compiled
                .into_iter()
                .map(|c| Bson::Document(doc! { "$ifNull": [c, ""] }))
                .collect();
            if parts.is_empty() {
                Bson::String(String::new())
            } else {
                Bson::Document(doc! { "$concat": parts })
            }
        }
        Function::Substr => {
            arity(func, args, 2..=3)?;
            let x = compiled.remove(0);
            let start = Bson::Document(doc! { "$subtract": [compiled.remove(0), 1_i64] });
            let len = if compiled.is_empty() {
                Bson::Int64(i64::from(i32::MAX))
            } else {
                compiled.remove(0)
            };
            let inner = Bson::Document(doc! { "$substrCP": [x.clone(), start, len] });
            null_guard(&x, inner)
        }
        Function::Replace => {
            arity(func, args, 3..=3)?;
            let x = compiled.remove(0);
            let from = compiled.remove(0);
            let to = compiled.remove(0);
            Bson::Document(doc! { "$replaceAll": { "input": x, "find": from, "replacement": to } })
        }
    })
}

fn cast(expr: &Expr, ty: SqlType, scope: &Scope<'_>) -> Res<Bson> {
    let input = expr_bson(expr, scope)?;
    let to = match ty {
        SqlType::SmallInt | SqlType::Integer | SqlType::BigInt => "long",
        SqlType::Real | SqlType::Double => "double",
        SqlType::Decimal => "decimal",
        SqlType::Bool => "bool",
        SqlType::Text => "string",
        SqlType::Timestamp => "date",
        other => {
            return Err(invalid(format!(
                "CAST to {other:?} is not supported by MongoDB"
            )));
        }
    };
    Ok(Bson::Document(
        doc! { "$convert": { "input": input, "to": to } },
    ))
}

/// Dates and times are stored as canonical text, so a string operand is
/// parsed first; timestamps are native dates.
fn as_date(x: &Bson) -> Bson {
    Bson::Document(doc! { "$cond": [
        { "$eq": [ { "$type": x.clone() }, "string" ] },
        { "$dateFromString": { "dateString": x.clone(), "onError": Bson::Null, "onNull": Bson::Null } },
        x.clone(),
    ] })
}

fn date_part(part: DatePart, x: &Bson) -> Res<Bson> {
    let date = as_date(x);
    let clock = |start: i64, native: &str| {
        // `HH:MM:SS.ffffff` time-of-day strings have ':' at index 2.
        Bson::Document(doc! { "$cond": [
            { "$and": [
                { "$eq": [ { "$type": x.clone() }, "string" ] },
                { "$eq": [ { "$indexOfCP": [x.clone(), ":"] }, 2_i64 ] },
            ] },
            { "$toInt": { "$substrCP": [x.clone(), start, 2_i64] } },
            { native: date.clone() },
        ] })
    };
    Ok(match part {
        DatePart::Year => Bson::Document(doc! { "$year": date }),
        DatePart::Month => Bson::Document(doc! { "$month": date }),
        DatePart::Day => Bson::Document(doc! { "$dayOfMonth": date }),
        DatePart::Week => Bson::Document(doc! { "$isoWeek": date }),
        DatePart::Quarter => Bson::Document(doc! { "$toInt": { "$ceil": { "$divide": [
            { "$month": date }, 3_i64
        ] } } }),
        DatePart::Hour => clock(0, "$hour"),
        DatePart::Minute => clock(3, "$minute"),
        DatePart::Second => clock(6, "$second"),
        DatePart::Date => {
            Bson::Document(doc! { "$dateToString": { "format": "%Y-%m-%d", "date": date } })
        }
    })
}
