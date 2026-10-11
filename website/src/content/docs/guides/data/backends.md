---
title: Backends
description: SQLite, PostgreSQL, MySQL, MongoDB, and Redis — capabilities, dialects, and how unsupported features fail.
---

Every QuerySet compiles to a backend-neutral `QueryPlan`. The backend
checks `required_features()` against `BackendCapabilities` **before any
I/O** and returns a typed `BackendCapabilityError` for anything it cannot
run. Nothing is silently ignored.

The compact matrix is in [Backend matrix](/siderite/reference/backend-matrix/). This
page covers how to connect, dialect notes, and Redis.

## Status

| Backend | Feature | Execution | Notes |
|---|---|---|---|
| SQLite | default | `SqliteBackend` | Row locking, regex, `DISTINCT ON`, arrays, `STDDEV` / `VARIANCE` are capability errors |
| PostgreSQL | `postgres` | `PgBackend` | Live suite against `postgres:17` |
| MySQL 8 | `mysql` | `MySqlBackend` | 8.0.31+; live suite against `mysql:8.4` |
| MongoDB | `mongodb` | `MongoBackend` | 5.0+; replica set for transactions; supported QuerySet subset |
| Redis | `redis` | `RedisStore` | Key/hash/set client. **Not** a QuerySet backend |

DynamoDB is a design note only; it is not planned for v1.

```rust
use siderite_backends::sqlite::SqliteBackend;
use siderite::orm::Db;

let db = Db::new(SqliteBackend::connect("sqlite::memory:").await?);
```

Live tests skip themselves when the matching URL variable is unset. Start
the four servers from the root `docker-compose.yml`. See
[Testing](/siderite/guides/production/testing/).

## Canonical storage

PostgreSQL stores every `Value` natively. SQLite has fewer storage classes,
so the adapter binds and expects the canonical forms from
`siderite_orm::types`. Decoding accepts both the native and the canonical
form.

| Rust type | PostgreSQL | SQLite | MySQL | MongoDB |
|---|---|---|---|---|
| `bool` | `boolean` | integer 0 / 1 | `TINYINT(1)` | boolean |
| `i16` / `i32` / `i64` | `smallint` / `integer` / `bigint` | integer | `SMALLINT` / `INT` / `BIGINT` | int64 |
| `f32` / `f64` | `real` / `double precision` | real | `FLOAT` / `DOUBLE` | double |
| `Decimal` | `numeric` | `NUMERIC` column; bound as text | `DECIMAL(p,s)` | Decimal128 |
| `Uuid` | `uuid` | text (hyphenated) | `CHAR(36)` | Binary subtype 4 |
| `NaiveDate` | `date` | text `YYYY-MM-DD` | `DATE` | text `YYYY-MM-DD` |
| `NaiveTime` | `time` | text `HH:MM:SS.ffffff` | `TIME(6)` | text `HH:MM:SS.ffffff` |
| `DateTime<Utc>` | `timestamptz` | text RFC3339 (fixed width) | `DATETIME(6)`, UTC session | BSON datetime (ms; µs truncated) |
| `serde_json::Value` | `jsonb` | text | `JSON` | embedded document |
| `Vec<u8>` | `bytea` | blob | `BLOB` | Binary |

Decimals on SQLite: a `TEXT` column would compare `"9.99"` after `"15.00"`.
With `NUMERIC` affinity SQLite stores numbers. Very long decimals lose
precision beyond a double.

Numeric results on PostgreSQL: `SUM` and `AVG` return `numeric`; integer
and float types decode a `Decimal`, so `row.get_as::<i64>("total")` works
on both backends.

Bulk operations chunk rows to `max_params` (SQLite 32766, PostgreSQL/MySQL
65535, MongoDB 50000 rows per `insert_many`).

## SQL dialect notes

- Placeholders are `$n` in PostgreSQL and `?` in SQLite and MySQL. The same
  in raw SQL. Parameters are numbered across the whole query, subqueries
  included.
- Identifiers are double-quoted on PostgreSQL/SQLite and backtick-quoted on
  MySQL.
- `LIKE` patterns escape `%`, `_`, and `\` and add `ESCAPE '\'`.
- SQLite `LIKE` is ASCII case-insensitive, so case-sensitive `contains` /
  `startswith` / `endswith` compile to `instr` and `substr`.
- An empty `IN` list compiles to `(1=0)`. An empty needle matches
  everything.
- SQLite needs a `LIMIT` before `OFFSET`, so it emits `LIMIT -1 OFFSET n`.
  MySQL emits `LIMIT 18446744073709551615` for a bare `OFFSET`.
- `concat` treats `NULL` as empty text.
- A `NULL` value is written as the keyword `NULL`, not bound, so statement
  text stays distinct for SQLx’s prepared-statement cache. In `raw_sql` a
  `Value::Null` is sent untyped; keep `NULL` and non-`NULL` variants of a
  raw statement textually different (`WHERE x IS NULL`) or cast the
  parameter.

## MySQL

- **`RETURNING` emulation.** Generated keys come from `LAST_INSERT_ID()`
  stepped by `@@auto_increment_increment`, then rows are re-read by key.
  Other updates and deletes read the affected keys `FOR UPDATE` inside a
  transaction. Tables need a primary key; an update with `RETURNING` may
  not change it.
- `connect` pins `time_zone '+00:00'`, removes `NO_BACKSLASH_ESCAPES`, and
  raises `group_concat_max_len`. A pool given to `from_pool` must do the
  same.
- Default collation is case-insensitive. `contains` / `startswith` /
  `endswith` / regex are forced case-sensitive. Use `utf8mb4_bin` for
  case-sensitive equality.
- `UPDATE .. SET` assignments are reordered so every right-hand side sees
  the old values. A cycle (`SET a = b, b = a`) is `InvalidPlan`.
- Subqueries with `LIMIT`, and subqueries over the target of an
  `UPDATE`/`DELETE`, are wrapped in derived tables. A correlated `EXISTS`
  over the target table still fails with MySQL 1093.
- DDL commits implicitly. `TEXT` cannot be a primary key: use
  `VARCHAR(191)`. See [Migrations](/siderite/guides/data/migrations/).

### Experimental native MySQL

Enable the `mysql-native` feature on `siderite-backends` and explicitly use
`mysql::native::NativeMySqlBackend`. SQLx `MySqlBackend` remains the default.
The adapter shares its canonical values and trigger-aware RETURNING engine.

```rust
use siderite_backends::mysql::native::NativeMySqlBackend;
use siderite::orm::Db;

let backend = NativeMySqlBackend::connect(&database_url).await?;
let db = Db::new(backend.clone());
// Release transaction handles before disconnecting the pool.
backend.close().await?;
```

`connect_with` accepts `NativeMySqlOptions`: defaults are 10 connections,
100 waiting callers and a 10-second checkout deadline. Startup initializes
all pool slots; `warm` repeats this after schema setup, before traffic starts.
Clean ORM sessions retain prepared statements. Raw SQL clears table metadata
and retires its session after use; transaction and schema scopes retain that
session until completion. Cancellation and unfinished transactions retire
their sockets, and interrupted handles cannot commit. Writes are never
automatically retried after a transport error or ambiguous commit.

Native URL settings use the pinned driver's syntax. TLS, server restart,
complete migration recovery and matched performance evidence remain release
gates. This adapter is experimental and has not replaced the default driver.

## MongoDB

- Requires MongoDB 5.0+. Transactions need a replica set (a single-node one
  is enough). No savepoints, so `bulk_create` inside a transaction is a
  capability error.
- A table is a collection. The primary key column (default `id`) is stored
  as `_id`. Missing keys are generated as consecutive `i64` values from
  `siderite_counters`, **outside** the transaction, so rollbacks leave gaps.
- Reads compile to aggregation pipelines. Plans are checked and compiled
  entirely before any I/O.
- Predicates follow SQL three-valued logic: `= NULL` means `IS NULL`.
- `update()` / `delete()` on a queryset with a limit, `distinct`, or joins
  becomes `pk IN (subquery)` and is therefore a `Subqueries` capability
  error.
- No foreign keys or cascades. Only `_id` is unique, plus indexes made with
  `MongoBackend::create_unique_index`.
- Joins, subqueries, set operations, window functions, row locking,
  `DISTINCT ON`, and array aggregates are capability errors (HTTP 501).

## Redis

`RedisStore` wraps a `ConnectionManager` with an optional key prefix: keys
(`get`, `set` with TTL, `set_nx`, `set_nx_px`, `del`, `exists`, `expire`, `ttl`,
`incr_by`), hashes, sets, `get_json` / `set_json`, and atomic `MULTI` /
`EXEC` pipelines. It does not implement QuerySet traits;
`BackendCapabilities::redis()` rejects every relational feature.

- Values are UTF-8; TTLs are milliseconds (`PX` / `PEXPIRE`), rejected
  below 1 ms. `set_nx` sets no TTL (`set_nx_px` creates with one in a single
  `SET NX PX`); `set` without a TTL clears an existing
  one.
- A command Redis rejects while queueing aborts the whole batch; a command
  failing during `EXEC` does not undo earlier ones.
- `pipeline` does not prefix keys: pass `store.key(..)`. `delete_namespace`
  matches `prefix*`, so end prefixes with a separator.
- Single server only: no cluster, pub/sub, lists, sorted sets, streams, or
  Lua.

`RedisCache` (feature `redis`) sits on this store. See
[Cache](/siderite/guides/production/cache/).

## Raw queries

`Db::raw_sql` returns a `QueryResult`. Decode with
`result.decode::<Model>()` (columns by name),
`result.decode_values::<(A, B)>()` (columns by position), or
`result.scalar()`. `Row::get_as::<T>(column)` decodes one value.
`Db::raw_execute` returns the affected row count. Values are always bound;
never format untrusted input into the SQL text.

MongoDB rejects raw SQL (`Feature::RawSql`); use
`MongoBackend::raw_command`.

## See also

- [Backend matrix](/siderite/reference/backend-matrix/)
- [QueryPlan IR](/siderite/internals/query-plan/)
- [Transactions](/siderite/guides/data/transactions/)

## Prefetch parameter admission

Prefetch constructs each source key's canonical identity once and uses a
hash set to deduplicate keys. Repeated references share loaded objects;
target queries are batched, without one query per source object. An explicit
prefetch queryset uses its own database's capabilities. SQL adapters count
its full compiled bind parameters before allocating the remaining IN-list
budget; NULL literals do not consume placeholders. Exhausted budgets fail
before target I/O. Empty target querysets skip parameter counting and I/O.

`Backend::read_parameter_count` returns an exact compiled read bind count;
SQL extension backends must implement it to support bounded prefetch.
`Db::read_parameter_count` exposes the same no-I/O diagnostic. Unknown SQL
counts fail explicitly. MongoDB batching uses its document batch capacity.
Sliced target querysets spanning multiple bind batches are rejected because
repeating a limit/offset per batch would change the queryset's meaning.

## SQLx connection initialization

PostgreSQL/MySQL `connect_with(url, options)` installs mandatory session
settings and replaces `options.after_connect`. SQLx keeps that callback
private, so a constructor cannot recover it for transparent composition.
Other pool settings, including checkout validation, remain in effect.
Use `connect_with_init(url, options, hook)` for custom session setup:

```rust
use siderite_backends::connection_init::ConnectionInit;
use siderite_backends::postgres::PgBackend;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;

let hook: ConnectionInit<sqlx::Postgres> = Arc::new(|conn, _| {
    Box::pin(async move {
        sqlx::Executor::execute(conn,
            "SET application_name = 'my-app'").await?;
        Ok(())
    })
});
let backend = PgBackend::connect_with_init(
    &database_url, PgPoolOptions::new(), hook,
).await?;
```

The custom hook runs first on every fresh/replacement connection. Required
UTC (and MySQL SQL-mode/GROUP_CONCAT) settings run afterwards, overriding
conflicting values. A hook must leave no transaction or lock open. Its
failure rejects that physical connection; SQLx retries within the pool
acquisition deadline and controls hook error logging. Callback capture
must be safe across concurrent connections. Wrapping an external pool with
`from_pool` leaves session initialization and reset obligations to its owner.
Native MySQL retains its separate explicit session/lease policy.

## Verified network transports

Enable `siderite-backends/tls` for SQLx/Redis Rustls support. PostgreSQL
uses `sslmode=verify-full` and a trusted `sslrootcert`; MySQL uses
`ssl-mode=VERIFY_IDENTITY` and `ssl-ca`. Preferred modes do not establish
verified peer identity and can permit plaintext fallback. Native MySQL
already compiles Rustls; its `connect_options` accepts `SslOpts` with private
roots while preserving mandatory adapter/pool settings. Redis uses `rediss`
and a configured Client plus `RedisStore::connect_with` for custom roots.
MongoDB already enables Rustls; keep certificate/hostname verification on.
SQLite has no network transport. Keep credentials and URLs out of logs.

The repository's `docs/BACKEND_TLS.md` documents deployment options and
`scripts/test_tls.py` owns the local positive/negative verification fixture.
The rsa exception is reviewed in `docs/DEPENDENCY_EXCEPTIONS.md`: the pinned
SQLx authentication path encrypts with the server's public key; no framework
private-key operation was found. This rationale does not cover application
RSA operations or substitute for verified TLS.
