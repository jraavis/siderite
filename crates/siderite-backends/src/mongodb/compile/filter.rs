//! Filters: the query language, `$expr` where an operator has no form.

use super::super::value::to_bson;
use super::expr::expr_bson;
use super::{Ctx, Res, Scope, invalid, unsupported};
use ::mongodb::bson::{Bson, Document, doc};
use siderite_orm::expr::{BinaryOp, Lookup, UnaryOp};
use siderite_orm::{Expr, Feature, Value};
fn false_doc() -> Document {
    doc! { "$expr": false }
}

/// SQL three-valued logic: a row passes only when the predicate is *true*.
/// `neg` compiles `NOT expr`, keeping rows with a NULL operand excluded
/// (MongoDB's `$ne`, `$nin`, `$not` and `$nor` would include them).
pub(super) fn filter_doc(expr: &Expr, neg: bool, ctx: &Ctx<'_>) -> Res<Document> {
    if let Some(doc) = query_form(expr, neg, ctx)? {
        return Ok(doc);
    }
    let e = if neg {
        Expr::Unary {
            op: UnaryOp::Not,
            expr: Box::new(expr.clone()),
        }
    } else {
        expr.clone()
    };
    Ok(doc! { "$expr": expr_bson(&e, &Scope::row(ctx))? })
}

fn children_filters(items: &[Expr], neg: bool, ctx: &Ctx<'_>) -> Res<Vec<Bson>> {
    items
        .iter()
        .map(|e| filter_doc(e, neg, ctx).map(Bson::Document))
        .collect()
}

fn column_field(expr: &Expr, ctx: &Ctx<'_>) -> Res<Option<String>> {
    match expr {
        Expr::Column(c) => ctx.field(c).map(Some),
        _ => Ok(None),
    }
}

fn flip(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::Lt => BinaryOp::Gt,
        BinaryOp::Le => BinaryOp::Ge,
        BinaryOp::Gt => BinaryOp::Lt,
        BinaryOp::Ge => BinaryOp::Le,
        other => other,
    }
}

fn negate_cmp(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::Lt => BinaryOp::Ge,
        BinaryOp::Le => BinaryOp::Gt,
        BinaryOp::Gt => BinaryOp::Le,
        BinaryOp::Ge => BinaryOp::Lt,
        BinaryOp::Eq => BinaryOp::Ne,
        BinaryOp::Ne => BinaryOp::Eq,
        other => other,
    }
}

fn cmp_key(op: BinaryOp) -> Option<&'static str> {
    Some(match op {
        BinaryOp::Lt => "$lt",
        BinaryOp::Le => "$lte",
        BinaryOp::Gt => "$gt",
        BinaryOp::Ge => "$gte",
        _ => return None,
    })
}

fn is_null_doc(field: &str, is_null: bool) -> Document {
    if is_null {
        doc! { field: Bson::Null }
    } else {
        doc! { field: { "$ne": Bson::Null } }
    }
}

/// `Some(document)` when `expr` has a query-language form.
#[allow(clippy::too_many_lines)]
fn query_form(expr: &Expr, neg: bool, ctx: &Ctx<'_>) -> Res<Option<Document>> {
    Ok(Some(match expr {
        Expr::And(items) if items.is_empty() => {
            if neg {
                false_doc()
            } else {
                Document::new()
            }
        }
        Expr::Or(items) if items.is_empty() => {
            if neg {
                Document::new()
            } else {
                false_doc()
            }
        }
        Expr::And(items) => {
            let children = children_filters(items, neg, ctx)?;
            if neg {
                doc! { "$or": children }
            } else {
                doc! { "$and": children }
            }
        }
        Expr::Or(items) => {
            let children = children_filters(items, neg, ctx)?;
            if neg {
                doc! { "$and": children }
            } else {
                doc! { "$or": children }
            }
        }
        Expr::Unary {
            op: UnaryOp::Not,
            expr,
        } => filter_doc(expr, !neg, ctx)?,
        Expr::Value(Value::Bool(b)) => {
            if *b != neg {
                Document::new()
            } else {
                false_doc()
            }
        }
        Expr::Value(Value::Null) => false_doc(),
        Expr::Column(c) => {
            let field = ctx.field(c)?;
            doc! { field: !neg }
        }
        Expr::Binary { op, lhs, rhs } => {
            let (field, op, value) = match (&**lhs, &**rhs) {
                (Expr::Column(c), Expr::Value(v)) => (ctx.field(c)?, *op, v),
                (Expr::Value(v), Expr::Column(c)) => (ctx.field(c)?, flip(*op), v),
                _ => return Ok(None),
            };
            let op = if neg { negate_cmp(op) } else { op };
            match (op, value) {
                (BinaryOp::Eq, Value::Null) => is_null_doc(&field, true),
                (BinaryOp::Ne, Value::Null) => is_null_doc(&field, false),
                // Comparing with NULL is never true, negated or not.
                (
                    BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod,
                    _,
                ) => {
                    return Ok(None);
                }
                (_, Value::Null) => false_doc(),
                (BinaryOp::Eq, v) => doc! { field: { "$eq": to_bson(v)? } },
                (BinaryOp::Ne, v) => {
                    // x <> v excludes NULL and missing values, like SQL.
                    doc! { field: { "$nin": [to_bson(v)?, Bson::Null] } }
                }
                (op, v) => match cmp_key(op) {
                    Some(key) => doc! { field: { key: to_bson(v)? } },
                    None => return Ok(None),
                },
            }
        }
        Expr::Lookup {
            expr: target,
            lookup,
        } => {
            let Some(field) = column_field(target, ctx)? else {
                return Ok(None);
            };
            match lookup_doc(&field, lookup, neg)? {
                Some(doc) => doc,
                None => return Ok(None),
            }
        }
        _ => return Ok(None),
    }))
}

pub(super) fn escape_regex(text: &str) -> Res<String> {
    if text.contains('\0') {
        return Err(invalid("a pattern cannot contain a NUL character"));
    }
    let mut out = String::with_capacity(text.len() + 4);
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii() {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    Ok(out)
}

pub(super) fn check_pattern(pattern: &str) -> Res<()> {
    if pattern.contains('\0') {
        return Err(invalid("a pattern cannot contain a NUL character"));
    }
    Ok(())
}

fn regex_bson(pattern: &str, insensitive: bool) -> Bson {
    Bson::RegularExpression(::mongodb::bson::Regex {
        pattern: pattern.into(),
        options: if insensitive {
            "i".into()
        } else {
            String::new()
        },
    })
}

fn regex_doc(field: &str, pattern: &str, insensitive: bool, neg: bool) -> Document {
    if neg {
        // `$not` alone would also match NULL and missing values.
        doc! { field: { "$ne": Bson::Null, "$not": regex_bson(pattern, insensitive) } }
    } else if insensitive {
        doc! { field: { "$regex": pattern, "$options": "i" } }
    } else {
        doc! { field: { "$regex": pattern } }
    }
}

fn lookup_doc(field: &str, lookup: &Lookup, neg: bool) -> Res<Option<Document>> {
    Ok(Some(match lookup {
        Lookup::IExact(Value::Text(text)) => {
            regex_doc(field, &format!("^{}\\z", escape_regex(text)?), true, neg)
        }
        Lookup::IExact(Value::Null) | Lookup::IsNull(true) => is_null_doc(field, !neg),
        Lookup::IExact(_) => return Ok(None),
        Lookup::Contains {
            needle,
            case_insensitive,
        } => regex_doc(field, &escape_regex(needle)?, *case_insensitive, neg),
        Lookup::StartsWith {
            needle,
            case_insensitive,
        } => regex_doc(
            field,
            &format!("^{}", escape_regex(needle)?),
            *case_insensitive,
            neg,
        ),
        Lookup::EndsWith {
            needle,
            case_insensitive,
        } => regex_doc(
            field,
            &format!("{}\\z", escape_regex(needle)?),
            *case_insensitive,
            neg,
        ),
        Lookup::Regex(pattern) => {
            check_pattern(pattern)?;
            regex_doc(field, pattern, false, neg)
        }
        Lookup::IsNull(false) => is_null_doc(field, neg),
        Lookup::In(values) => {
            let mut list: Vec<Bson> = values
                .iter()
                .filter(|v| !v.is_null())
                .map(to_bson)
                .collect::<Result<_, _>>()?;
            if neg {
                list.push(Bson::Null);
                doc! { field: { "$nin": list } }
            } else if list.is_empty() {
                false_doc()
            } else {
                doc! { field: { "$in": list } }
            }
        }
        Lookup::Range(lo, hi) => {
            if lo.is_null() || hi.is_null() {
                false_doc()
            } else if neg {
                doc! { "$or": [
                    { field: { "$lt": to_bson(lo)? } },
                    { field: { "$gt": to_bson(hi)? } },
                ] }
            } else {
                doc! { field: { "$gte": to_bson(lo)?, "$lte": to_bson(hi)? } }
            }
        }
        Lookup::InSubquery(_) => return Err(unsupported(Feature::Subqueries)),
    }))
}
