---
title: QuerySets
description: Lazy QuerySet builders, lookups, annotations, aggregates, pagination, and writes.
---

A `QuerySet<M>` is a lazy, immutable query over one model. Builder methods
consume and return the queryset and never perform I/O. Terminal methods
(`all`, `get`, `count`, `update`, …) run the compiled `QueryPlan` on the
queryset’s `Db`. Clone a queryset to branch from a common base.

```rust
let adults = User::objects(&db)
    .filter(User::age.ge(18).and(User::name.icontains("ann")))
    .order_by([User::created_at.desc()])
    .limit(20)
    .all()
    .await?;
```

Mistakes that can only be detected while building (an unknown annotation
name, a filter on a window function) are remembered and returned by the
next terminal method, so builder chains stay infallible.

The method list with signatures is in [QuerySet API](/siderite/reference/queryset/).

## Lookups

`#[derive(Model)]` generates `User::name: Field<User, String>`. Lookups are
inherent methods that exist only where they make sense:

| Method | Typical types |
|---|---|
| `eq`, `ne`, `lt`, `le`, `gt`, `ge` | comparable |
| `in_iter`, `in_subquery` | any |
| `is_null` | any (or `eq(None::<T>)`, which compiles to `IS NULL`) |
| `contains`, `startswith`, `endswith` | strings |
| `icontains`, `istartswith`, `iendswith`, `iexact` | strings, case-insensitive |
| `regex` | strings; capability `Regex` |
| `range` | comparable |

Boolean combination is `.and()` / `.or()` on `Expr`. Related traversal is
`Post::author.join(Author::name)` — a field of `Post` typed `String`,
resolved into a `JOIN` when compiled. See [Relations](/siderite/guides/data/relations/).

## Reads

| Method | Result |
|---|---|
| `all()` | `Vec<M>` |
| `all_annotated()` | `Vec<(M, Row)>` when annotations are present |
| `first()` / `last()` | `Option<M>` |
| `earliest(field)` / `latest(field)` | `M` |
| `get(predicate)` | `M`; `DoesNotExist` or `MultipleObjectsReturned` |
| `count()` | `u64` |
| `exists()` | `bool` |
| `contains(&object)` | `bool` (by primary key) |
| `paginate(page, per_page)` | `Page<M>` with `total_pages()` |
| `after(token)` / `before(token)` + `cursor_page(n)` | `CursorPage<M>` (`next`, `previous`) |
| `in_bulk(keys)` | map of primary key → model |
| `rows()` | `Vec<Row>` |
| `values(fields)` | `Vec<Row>` of named columns |
| `values_list::<T>(fields)` | decoded tuples |
| `aggregate(fields)` | a single `Row` of aggregates |

`count`, `exists`, and `aggregate` drop ordering and row locks (PostgreSQL
rejects `ORDER BY` next to a bare aggregate). A plan with limit, offset,
distinct, grouping, or set operations is wrapped:
`SELECT COUNT(*) FROM (..) AS "counted"`.

## Filtering, ordering, paging

```rust
User::objects(&db)
    .filter(User::age.ge(18))
    .exclude(User::name.eq("admin"))
    .order_by([User::name.asc()])
    .reverse()
    .limit(10)
    .offset(20)
    .distinct()
```

- `none()` makes every terminal return empty without touching the database.
- `using(&db)` rebinds the queryset to a handle you already hold.
- `distinct_on([User::name])` needs `Feature::DistinctOn` (PostgreSQL).
- `select_for_update()`, `nowait()`, `skip_locked()` need row locking and
  belong inside `Db::transaction`. See [Transactions](/siderite/guides/data/transactions/).

## Cursor pagination

`paginate` counts and uses `OFFSET`. For long or fast-changing lists use
`cursor_page`, which seeks past the last row of the previous page:

```rust
let qs = Book::objects(&db).order_by([Book::likes.desc()]);
let page = qs.clone().cursor_page(20).await?;
let more = qs.clone().after(&page.next.unwrap()).cursor_page(20).await?;
let back = qs.before(&more.previous.unwrap()).cursor_page(20).await?;
```

- One statement per page (`LIMIT n + 1`, no count). `CursorPage` has
  `items`, `next` and `previous`; `None` marks the last and first page.
- The primary key is appended to the ordering, so every row has a distinct
  position. Ordering terms must be plain model columns; `distinct`,
  grouping, set operations, `limit`/`offset` and row locks are rejected.
- Nulls sort last ascending and first descending on every backend.
- A token is bound to the table, filters and ordering that issued it. A bad
  or foreign token is `OrmError::Cursor` with code `cursor.E001` to
  `cursor.E004` (HTTP 400). Tokens are validated but not signed.
- PostgreSQL, MySQL and SQLite are supported; MongoDB pages non-null
  ordering columns only; Redis fails with a capability error.
- Rows that exist for the whole iteration are returned exactly once, however
  rows are inserted or deleted meanwhile.

For the HTTP side see [Extractors](/siderite/guides/http/extractors/#cursor-pagination).

## Annotations and grouping

`annotate("n", expr)` / `alias("n", expr)` name an expression. Later
`filter`, `order_by`, and `project` calls refer to it with `Expr::col("n")`.
The expression is inlined, because PostgreSQL does not allow output aliases
in `WHERE` or `HAVING`.

A predicate containing an aggregate becomes `HAVING`. When the projection
contains aggregates, the other projected expressions become `GROUP BY` and
the model’s default ordering is dropped. Group with
`project([..]).annotate(..)`. A filter on a window function is an error.

```rust
use siderite::orm::expr::Aggregate;

let row = User::objects(&db)
    .aggregate([Aggregate::Count.star().alias("n")])
    .await?;
```

## Set operations

`union`, `union_all`, `intersection`, and `difference` take another
`QuerySet<M>` and return `Result`. Combining querysets bound to different
databases is `QueryError::InvalidPlan` before any SQL is sent. A
transaction handle counts as its pool.

## Writes

| Method | Result |
|---|---|
| `create(object)` | insert and return `M` (fills an `auto` pk) |
| `get_or_create(defaults, object)` | `(M, created)` |
| `update_or_create(...)` | update or insert |
| `update([(field, value), ...])` | `u64` rows |
| `delete()` | `u64` rows |
| `bulk_create(objects)` | `Vec<M>` |
| `bulk_update(objects, fields)` | `u64` |

A queryset with joins, a limit, or distinct becomes
`WHERE pk IN (SELECT pk ..)` for `update` / `delete`. Bulk operations and
queryset `update` / `delete` send **no signals** — they never load
instances. See [Signals](/siderite/guides/data/signals/).

## Subqueries

`QuerySet::subquery(field)` and `exists_expr()` stamp the plan with the
database they were built on. Running a query or bulk write that contains a
subquery from another database fails with `QueryError::InvalidPlan` before
any SQL is sent.

## See also

- [QuerySet API](/siderite/reference/queryset/)
- [QueryPlan IR](/siderite/internals/query-plan/)
- [Backends](/siderite/guides/data/backends/)
