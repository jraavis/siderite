# Backends

## Status

| Backend | SQL compile | Execution | Notes |
|---|---|---|---|
| SQLite | yes | yes (`SqliteBackend`, SQLx) | Row locking, regex, `DISTINCT ON`, arrays and `STDDEV` / `VARIANCE` are rejected with a capability error |
| PostgreSQL | yes | yes (`PgBackend`, SQLx, feature `postgres`), live suite passed against `postgres:17` on 2026-09-29 | `tests/postgres.rs` runs when `DATABASE_URL` starts with `postgres` and is skipped otherwise. Example: `docker run -d -e POSTGRES_PASSWORD=postgres -p 5432:5432 postgres:17` then `DATABASE_URL=postgres://postgres:postgres@localhost/postgres cargo test -p siderite-backends --all-features --test postgres` |
| MySQL | yes | yes (`MySqlBackend`, SQLx, feature `mysql`), MySQL 8.0.31+, live suite passed against `mysql:8.4` on 2026-09-29 | `tests/mysql.rs` runs when `MYSQL_URL` (or `DATABASE_URL`) starts with `mysql`. Example: `MYSQL_URL=mysql://root@127.0.0.1:3306/siderite_test cargo test -p siderite-backends --all-features --test mysql` |
| MongoDB | subset, to aggregation pipelines | yes (`MongoBackend`, feature `mongodb`), MongoDB 5.0+, replica set for transactions, live suite passed against `mongo:8` as a single-node replica set on 2026-09-29 | `tests/mongodb.rs` runs when `MONGODB_URL` starts with `mongodb` |
| Redis | not applicable | yes (`RedisStore`, feature `redis`) | A key/hash/set API; **not** a QuerySet backend. Live suite passed against `redis:7` on 2026-09-29. `tests/redis.rs` runs when `REDIS_URL` starts with `redis` (use db 15) |
| DynamoDB | not applicable | not applicable | Design note only; not planned for v1 |

### Running the live suites

The PostgreSQL, MySQL, MongoDB and Redis suites were run and passed against docker containers (`postgres:17`, `mysql:8.4`, `mongo:8` as a replica set, `redis:7`) on 2026-09-29. Reproduce with the root `docker-compose.yml`; the variables, the MySQL root requirement and the MongoDB replica set are described in [TESTING.md](TESTING.md#live-database-tests). SQLite suites need no setup.

## Feature matrix (implemented)

| Feature | PostgreSQL | SQLite | MySQL | MongoDB |
|---|---|---|---|---|
| Filtering, ordering, limit, offset, distinct | yes | yes | yes | yes |
| `DISTINCT ON` | yes | capability error | capability error | capability error |
| Joins (`join`, `select_related`) | yes | yes | yes | capability error (`Joins`) |
| Subqueries (`Exists`, `Subquery`, `in` subquery) | yes | yes | yes | capability error (`Subqueries`) |
| Transactions, savepoints | yes | yes | yes | flat transactions only |
| Isolation levels | read committed, repeatable read, serializable | serializable only | all three | none requestable |
| Row locking (`select_for_update`) | yes, plus `nowait`, `skip_locked` | capability error before any I/O | yes, plus `nowait`, `skip_locked` | capability error |
| Window functions | yes | yes (3.25+) | yes | capability error |
| Aggregates: count, sum, avg, min, max | yes | yes | yes | yes (`$group`) |
| `StdDev`, `Variance` | yes | capability error (`StatisticalAggregates`) | yes | yes |
| `StringAgg` | `STRING_AGG` | `group_concat` (no `DISTINCT`) | `GROUP_CONCAT(.. SEPARATOR ..)` | `$push` + `$reduce` (order unspecified) |
| `ArrayAgg` | yes (decoded as a JSON array) | capability error (`Arrays`) | capability error | capability error |
| Regex lookup | `~` | capability error | `REGEXP_LIKE(x, ?, 'c')` | `$regex` |
| Case-insensitive lookups | `ILIKE` | `LOWER(x) LIKE LOWER(?)` | `LOWER(x) LIKE LOWER(?)` | `$regex` with `i` |
| Set operations | yes | yes | yes (`INTERSECT`/`EXCEPT` need 8.0.31) | capability error (`SetOperations`) |
| `RETURNING` | yes | yes (3.35+) | emulated by the adapter | emulated by the adapter |
| Raw SQL | yes | yes | yes | capability error (`RawSql`); use `MongoBackend::raw_command` |
| Bind parameter limit (`max_params`) | 65535 | 32766 | 65535 | 50000 rows per `insert_many` |

Bulk operations chunk their rows to `max_params`, so a 10 000 row `bulk_create` of a seven-column model is three `INSERT`s on SQLite and two on PostgreSQL.

## Canonical storage forms

PostgreSQL stores every `Value` natively. SQLite has fewer storage classes, so the adapter binds and expects the canonical forms from `siderite_orm::types`. Decoding accepts both the native and the canonical form.

| Rust type | PostgreSQL | SQLite | MySQL | MongoDB |
|---|---|---|---|---|
| `bool` | `boolean` | integer 0 / 1 | `TINYINT(1)` | boolean |
| `i16` / `i32` / `i64` | `smallint` / `integer` / `bigint` | integer | `SMALLINT` / `INT` / `BIGINT` | int64 |
| `f32` / `f64` | `real` / `double precision` | real | `FLOAT` / `DOUBLE` | double |
| `Decimal` | `numeric` | declare the column `NUMERIC`; bound as text, which SQLite converts to a number for comparison and `SUM` | `DECIMAL(p,s)` | Decimal128 |
| `Uuid` | `uuid` | text (hyphenated) | `CHAR(36)`, hyphenated | Binary subtype 4 |
| `NaiveDate` | `date` | text `YYYY-MM-DD` | `DATE` | text `YYYY-MM-DD` |
| `NaiveTime` | `time` | text `HH:MM:SS.ffffff` | `TIME(6)` | text `HH:MM:SS.ffffff` |
| `DateTime<Utc>` | `timestamptz` | text `YYYY-MM-DDTHH:MM:SS.ffffffZ` (fixed width, so text order is time order) | `DATETIME(6)`, UTC session | BSON datetime (milliseconds; microseconds truncated) |
| `serde_json::Value` | `jsonb` | text | `JSON` | embedded document (scalars as JSON text) |
| `Vec<u8>` | `bytea` | blob | `BLOB` | Binary |

Two details matter in practice:

* **Decimals on SQLite.** A `TEXT` column would compare and aggregate `"9.99"` after `"15.00"`. With `NUMERIC` affinity SQLite stores numbers, and the ORM reads them back through `Decimal::from_value`, which accepts integers and floats. Very long decimals lose precision beyond a double.
* **Numeric results on PostgreSQL.** `SUM` and `AVG` return `numeric`; integer and float types decode a `Decimal` (integral, respectively any), so `row.get_as::<i64>("total")` works on both backends.

## Dialect notes

* Placeholders are `$n` in PostgreSQL and `?` in SQLite, and the same in raw SQL: `db.raw_sql("SELECT .. WHERE id = ?", params![id])` on SQLite, `... WHERE id = $1` on PostgreSQL. Parameters are numbered across the whole query, subqueries included.
* Identifiers are always double-quoted, and any embedded `"` is doubled.
* `LIKE` patterns escape `%`, `_` and `\` and add `ESCAPE '\'`.
* SQLite `LIKE` is ASCII case-insensitive, so case-sensitive `contains`, `startswith` and `endswith` compile to `instr` and `substr` there.
* An empty `IN` list compiles to `(1=0)`. An empty needle matches everything.
* SQLite needs a `LIMIT` before `OFFSET`, so it emits `LIMIT -1 OFFSET n`.
* Date parts: PostgreSQL uses `CAST(EXTRACT(.. FROM x) AS BIGINT)` (the adapter pins each connection to UTC); SQLite uses `CAST(strftime(..) AS INTEGER)` on the canonical text forms. `week` is the ISO 8601 week on both, computed on SQLite from the Thursday of the week. `quarter` is derived from the month there.
* `concat` treats `NULL` as empty text on both: PostgreSQL `CONCAT`, SQLite `COALESCE(CAST(x AS TEXT), '') || ..`.
* Integer literals are bound as 64-bit integers. PostgreSQL functions that take `integer` (`SUBSTR`, `NTILE`, `LAG`) get a `CAST` or a literal from the compiler. `LAG(x, n, default)` needs `default` to have the column's exact type; cast it when the column is not `bigint`.
* A `NULL` value is written as the keyword `NULL`, not bound, so the server infers its type from context and SQLx's statement cache (keyed by SQL text) never reuses a statement that was prepared with an inferred parameter type for a later value. In `raw_sql` a `Value::Null` parameter is sent untyped (OID 0); keep `NULL` and non-`NULL` variants of a raw statement textually different (`WHERE x IS NULL`) or cast the parameter (`$1::text`).

## SQLite notes

* **Write serialization (`WriteGate`):** SQLite allows only one writer per database file. In a pooled backend, concurrent writes on separate connections contend for the file lock, causing losing connections to sleep in SQLite's busy handler (1, 2, 5 … 100 ms backoff) and driving up tail latency. `SqliteBackend` serializes all writes across its clones through an in-process FIFO `WriteGate`. Uncontended writes proceed immediately; queued writers hand over the write slot without sleeping in the busy handler. Configure the maximum wait time with [`with_write_timeout`](file:///Users/raavi/dev/siderite/crates/siderite-backends/src/sqlite.rs) (default: 5 seconds, matching SQLx's busy timeout).
* **Group commit (`group_commit`):** Autocommit writes each normally pay a commit sync (multiple `fsync` calls in rollback-journal mode). With [`SqliteBackend::group_commit(GroupCommit::default())`](file:///Users/raavi/dev/siderite/crates/siderite-backends/src/sqlite.rs), concurrent writes queued during a flush share a single transaction and commit `fsync`. Each write runs within its own savepoint so individual errors fail only their caller, and responses are sent only after the shared commit completes, preserving durability.
* **Pool sizing and WAL:** [`connect`](file:///Users/raavi/dev/siderite/crates/siderite-backends/src/sqlite.rs) defaults to a pool of 10 connections for file databases and 1 for `:memory:`. Use [`connect_with`](file:///Users/raavi/dev/siderite/crates/siderite-backends/src/sqlite.rs) to customize pool size or opt in to `SqliteJournalMode::Wal` for concurrent non-blocking reads and writes.

## MySQL notes

* Identifiers use backticks (embedded backticks doubled); placeholders are `?`. A bare `OFFSET` gets `LIMIT 18446744073709551615`.
* **`RETURNING` emulation.** Generated keys come from `LAST_INSERT_ID()` stepped by `@@auto_increment_increment` (keys of one multi-row insert are consecutive), then rows are re-read by key. Other updates and deletes read the affected keys `FOR UPDATE` inside a transaction. Tables need a primary key; an update with `RETURNING` may not change it. Primary keys are looked up in `information_schema` and cached; the cache is cleared by `execute_script`.
* `connect` pins `time_zone '+00:00'`, removes `NO_BACKSLASH_ESCAPES` and raises `group_concat_max_len`. A pool given to `from_pool` must do the same.
* The default collation is case-insensitive, so plain equality, `DISTINCT`, ordering and unique constraints are too. `contains`, `startswith`, `endswith` and regex are forced case-sensitive (`LIKE CAST(? AS BINARY)`, `REGEXP_LIKE(.., 'c')`). Use a `utf8mb4_bin` column for case-sensitive equality.
* MySQL evaluates `UPDATE .. SET` assignments left to right; the compiler reorders them so every right-hand side sees the old values, as on PostgreSQL and SQLite. A cycle (`SET a = b, b = a`) is `InvalidPlan`.
* `FILTER (WHERE ..)` compiles to `AGG(CASE WHEN .. END)`; `CONCAT` wraps arguments in `COALESCE(x, '')`; `length` is `CHAR_LENGTH`; ISO week is `WEEK(x, 3)`; decimal casts are `DECIMAL(38,10)`.
* Subqueries with `LIMIT`, and subqueries over the target of an `UPDATE`/`DELETE`, are wrapped in derived tables (errors 1235 and 1093). A correlated `EXISTS` over the target table still fails with 1093.
* Integer `SUM`, `AVG` and `/` return `DECIMAL`; typed decodes to integers work. DDL commits implicitly. `TEXT` cannot be a primary key: use `VARCHAR(191)`.

## MongoDB notes

* Requires MongoDB 5.0+; transactions need a replica set (a single-node one is enough). No savepoints, so `bulk_create` inside a transaction is a capability error.
* A table is a collection. The primary key column (default `id`; register others with `MongoBackend::with_keys(Keys::default().with(collection, column))`) is stored as `_id` and renamed back on read. Missing keys are generated as consecutive `i64` values from the `siderite_counters` collection, outside the transaction, so rollbacks leave gaps; explicit keys do not advance the counter.
* Reads compile to aggregation pipelines (`$match`, `$group`, `$sort`, `$skip`, `$limit`, `$project`). Plans are checked and compiled entirely before any I/O; see the module docs of `siderite_backends::mongodb` for the full mapping of plan nodes.
* Predicates follow SQL three-valued logic: negations carry explicit non-`NULL` guards, and `= NULL` means `IS NULL`.
* `update()`/`delete()` on a queryset with a limit, `distinct` or joins becomes `pk IN (subquery)` and is therefore a `Subqueries` capability error.
* No foreign keys or cascades; only `_id` is unique, plus indexes made with `MongoBackend::create_unique_index`. Duplicate-key and validation failures map to `BackendError::Constraint`.
* `Update`/`Delete` with `RETURNING` pin the matching ids first; outside a transaction a concurrent writer can interleave.

## Redis notes

`RedisStore` (feature `redis`) wraps a `ConnectionManager` with an optional key prefix: keys (`get`, `set` with TTL, `set_nx`, `del`, `exists`, `expire`, `ttl`, `incr_by`), hashes, sets, `get_json`/`set_json`, and atomic `MULTI`/`EXEC` pipelines. It does not implement the QuerySet traits; `BackendCapabilities::redis()` rejects every relational feature.

* Values are UTF-8; TTLs are milliseconds (`PX`/`PEXPIRE`), rejected below 1 ms. `set_nx` sets no TTL; `set` without a TTL clears an existing one.
* A command Redis rejects while queueing aborts the whole batch; a command failing during `EXEC` does not undo earlier ones.
* `pipeline` does not prefix keys: pass `store.key(..)`. `delete_namespace` matches `prefix*`, so end prefixes with a separator.
* Single server only: no cluster, pub/sub, lists, sorted sets, streams or Lua.

## Locking notes

* `select_for_update()` keeps locks until the surrounding transaction ends; use it inside `Db::transaction`. Outside one, PostgreSQL releases the locks immediately.
* PostgreSQL refuses `FOR UPDATE` on the nullable side of an outer join, so do not combine it with `select_related` or filters through nullable foreign keys.

## Raw queries

`Db::raw_sql` returns a `QueryResult`; decode it with `result.decode::<Model>()` (columns by name), `result.decode_values::<(A, B)>()` (columns by position) or `result.scalar()`. `Row::get_as::<T>(column)` decodes one value. `Db::raw_execute` returns the affected row count. Values are always bound; never format untrusted input into the SQL text.

## Deferred

* Reverse foreign-key and many-to-many `prefetch_related` (the model struct has no slot to hold the children); use `ManyToManyManager::queryset()` or a filtered `QuerySet` for those.
* Multi-hop `prefetch_related` (use `select_related`).
* Window frames (`ROWS BETWEEN ..`).
* `INNER JOIN` for non-null foreign keys (all traversals use `LEFT JOIN`, which returns the same rows for them).
* `ArrayAgg` on MySQL (`JSON_ARRAYAGG` has no `DISTINCT`).
* MongoDB joins (`$lookup`), subqueries and window functions (`$setWindowFields`).
