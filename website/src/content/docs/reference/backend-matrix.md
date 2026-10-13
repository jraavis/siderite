---
title: Backend matrix
description: QuerySet feature support on PostgreSQL, SQLite, MySQL, and MongoDB.
---

Unsupported features fail with a `BackendCapabilityError` before any I/O.
Narrative notes: [Backends](/siderite/guides/data/backends/). Redis is a key/hash/set
client and is not in this table.

| Feature | PostgreSQL | SQLite | MySQL | MongoDB |
|---|---|---|---|---|
| Filtering, ordering, limit, offset, distinct | yes | yes | yes | yes |
| `DISTINCT ON` | yes | capability error | capability error | capability error |
| Joins (`join`, `select_related`) | yes | yes | yes | capability error (`Joins`) |
| Subqueries (`Exists`, `Subquery`, `in` subquery) | yes | yes | yes | capability error (`Subqueries`) |
| Transactions, savepoints | yes | yes | yes | flat transactions only |
| Isolation levels | read committed, repeatable read, serializable | serializable only | all three | none requestable |
| Row locking (`select_for_update`) | yes, plus `nowait`, `skip_locked` | capability error | yes, plus `nowait`, `skip_locked` | capability error |
| Window functions | yes | yes (3.25+) | yes | capability error |
| Aggregates: count, sum, avg, min, max | yes | yes | yes | yes (`$group`) |
| `StdDev`, `Variance` | yes | capability error (`StatisticalAggregates`) | yes | yes |
| `StringAgg` | `STRING_AGG` | `group_concat` (no `DISTINCT`) | `GROUP_CONCAT(.. SEPARATOR ..)` | `$push` + `$reduce` (order unspecified) |
| `ArrayAgg` | yes (decoded as a JSON array) | capability error (`Arrays`) | capability error | capability error |
| Regex lookup | `~` | capability error | `REGEXP_LIKE(x, ?, 'c')` | `$regex` |
| Case-insensitive lookups | `ILIKE` | `LOWER(x) LIKE LOWER(?)` | `LOWER(x) LIKE LOWER(?)` | `$regex` with `i` |
| Set operations | yes | yes | yes (`INTERSECT`/`EXCEPT` need 8.0.31) | capability error (`SetOperations`) |
| Cursor pagination (`cursor_page`) | yes | yes | yes | non-null ordering columns only (`NullableKeyset`); Redis: capability error (`KeysetPagination`) |
| `RETURNING` | yes | yes (3.35+) | emulated by the adapter | emulated by the adapter |
| Raw SQL | yes | yes | yes | capability error (`RawSql`); use `MongoBackend::raw_command` |
| Bind parameter limit (`max_params`) | 65535 | 32766 | 65535 | 50000 rows per `insert_many` |
| Schema migrations | yes | yes | yes | unsupported |

## Not yet

- Reverse foreign-key and many-to-many `prefetch_related` (no slot on the
  model struct for the children)
- Multi-hop `prefetch_related` (use `select_related`)
- Window frames (`ROWS BETWEEN ..`)
- `INNER JOIN` for non-null foreign keys (all traversals use `LEFT JOIN`)
- `ArrayAgg` on MySQL
- MongoDB joins (`$lookup`), subqueries, and window functions

## See also

- [Backends](/siderite/guides/data/backends/)
- [Migrations](/siderite/guides/data/migrations/)
- [Transactions](/siderite/guides/data/transactions/)
