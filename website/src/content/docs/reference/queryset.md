---
title: QuerySet API
description: Builder and terminal methods on QuerySet, with what each returns.
---

`QuerySet<M>` is a lazy query over one model. Builders consume `self` and
return `Self` (or `Result<Self, QueryError>` for set operations). Terminals
are `async` and talk to the queryset’s `Db`. Clone to branch.

Narrative guide: [QuerySets](/siderite/guides/data/querysets/).

## Construction

| Method | Returns | Notes |
|---|---|---|
| `M::objects(&db)` | `QuerySet<M>` | default ordering from `ModelMeta` |
| `using(&Db)` | `Self` | rebind to a handle you already hold |
| `db()` | `&Db` | |
| `plan()` | `&QueryPlan` | |

## Builders

| Method | Notes |
|---|---|
| `filter(Expr)` | AND with the existing predicate |
| `exclude(Expr)` | AND NOT |
| `order_by([OrderExpr])` | replaces default ordering |
| `reverse()` | flip direction |
| `limit(u64)` / `offset(u64)` | |
| `distinct()` | `DISTINCT` |
| `distinct_on([Expr])` | PostgreSQL; capability `DistinctOn` |
| `none()` | terminals return empty without I/O |
| `aliased(name)` | name the source for correlated subqueries |
| `select_for_update()` / `nowait()` / `skip_locked()` | row locking |
| `annotate(name, Expr)` / `alias(name, Expr)` | named expressions; later `Expr::col("name")` |
| `project([SelectExpr])` | restrict / reshape the SELECT list |
| `select_related(Relation)` | JOIN and fill FK cache |
| `prefetch_related(Prefetch)` | follow-up query |
| `union` / `union_all` / `intersection` / `difference` | `Result<Self, QueryError>`; same database only |

## Read terminals

| Method | Returns |
|---|---|
| `all()` | `Vec<M>` |
| `all_annotated()` | `Vec<(M, Row)>` |
| `first()` / `last()` | `Option<M>` |
| `earliest(field)` / `latest(field)` | `M` |
| `get(Expr)` | `M` (`DoesNotExist` / `MultipleObjectsReturned`) |
| `count()` | `u64` |
| `exists()` | `bool` |
| `contains(&M)` | `bool` |
| `paginate(page, per_page)` | `Page<M>` (`total_pages()`) |
| `cursor_page(limit)` after `after(token)` / `before(token)` | `CursorPage<M>` (`items`, `next`, `previous`) |
| `in_bulk(keys)` | map pk → model |
| `rows()` | `Vec<Row>` |
| `values([SelectExpr])` | `Vec<Row>` |
| `values_list::<T>([SelectExpr])` | decoded tuples |
| `aggregate([SelectExpr])` | `Row` |

## Write terminals

| Method | Returns |
|---|---|
| `create(M)` | `M` (fills `auto` pk) |
| `get_or_create(...)` | `(M, bool)` |
| `update_or_create(...)` | `M` |
| `update([(field, value), ...])` | `u64` |
| `delete()` | `u64` |
| `bulk_create(Vec<M>)` | `Vec<M>` |
| `bulk_update(objects, fields)` | `u64` |

Bulk writes and queryset `update` / `delete` send no signals.

## Subqueries

| Method | Returns |
|---|---|
| `subquery(field)` | `QueryPlan` stamped with this queryset’s database |
| `exists_expr()` | `Expr` (`EXISTS`) |

A plan whose nested subquery was built against another database is
`QueryError::InvalidPlan` before any SQL is sent.

## See also

- [QuerySets](/siderite/guides/data/querysets/)
- [Relations](/siderite/guides/data/relations/)
- [QueryPlan IR](/siderite/internals/query-plan/)
