# Changelog

## [Unreleased]

- Own HTTP/HTTP2 and WebSocket work through shutdown, with bounded transport
  admission and readiness phases; overdue cooperative workers are aborted
  and joined before resource teardown.


### Changed
- **Breaking:** MSRV is Rust 1.99, the stable compiler CI already uses.
- **Breaking (Migrations):** bounded PostgreSQL/MySQL advisory acquisition and read-only
  recovery inspection. Pending-step intents reject automatic replay of
  uncertain non-transactional scripts/callbacks. Callback replay requires
  an explicit idempotence declaration; execution requires a pool handle.
- **Benchmarks:** validate actual scheduler counts, paired confidence bounds,
  per-pair tail budgets with rounding uncertainty, and retained percentile
  CSV. SQLite FastAPI now has a retained autocommit connection baseline.
- **Security docs:** recommend authorization code with PKCE and trusted
  access-token verification; label password-flow examples as legacy.
- **ORM:** prefetch hash-deduplicates canonical keys and batches against
  the target database's remaining compiled bind budget. SQL extensions
  report counts with `Backend::read_parameter_count`; unknown/exhausted
  budgets and sliced queries spanning batches fail before target I/O.
- **Breaking (Core):** ConcurrencyLimit now defaults to a bounded queue
  with a five-second admission deadline. Overload returns 503/Retry-After.
  Configure `queue`, opt into body-lifetime permits with `hold_body`, close
  admission explicitly and observe counters with `stats`.
- **Cache:** enforce request refresh/no-store, conditional/range bypass,
  origin freshness and successful target-write invalidation. Namespaced
  random generations prevent delayed fills and eviction from reviving old
  entries. Failed invalidation disables the layer. MemoryCache adds hard
  byte admission and size-aware eviction; route encoding is bounded and
  versioned. Shared proxy policy resolves cache scheme.
- **Breaking (Middleware):** HTTPS redirects ignore forwarded scheme by
  default. Shared `TrustedProxies` enables exact-peer trust for scheme and
  rate-limit identity; malformed trusted headers are rejected. Redirects
  preserve IPv6 brackets and support canonical authority/host validation.
- **Middleware:** rate limits now enforce a hard client-state bound with
  typed normalized IP keys and bounded expiry. Saturation denies new
  identities without evicting depleted clients. Invalid numeric settings
  fail app validation; `try_new` supports immediate validation.
- **Breaking (Core):** dropping `BackgroundTasks` no longer launches work.
  Extracted tasks require a successful endpoint response; cancellation,
  extraction failure, errors and panic discard queued work. Admission is
  bounded, accepted batches retain tracing context and lifespan shutdown
  drains them before resource teardown. `try_add` reports overload; configure
  limits with `App::background_tasks`. Standalone defaults reject admission.
- **MySQL:** share metadata and RETURNING reconstruction through a private
  driver interface; compile emulated writes without cloning the whole plan.
  Existing SQLx constructors and stored-row behavior are preserved.
- **Breaking (ORM):** overlapping statements and sibling savepoint scopes on
  one transaction now return `TransactionBusy`. Use the active child handle
  and await each scope before starting another. Cancelling scope work returns
  `TransactionAborted` from later operations and prevents outer commit.
- Site mark is a rhombohedral crystal (header logos and favicon), replacing the old triangle.
- GitHub Actions use Node 24-capable versions (`actions/checkout@v5`, `upload-pages-artifact@v5`, `deploy-pages@v5`).
- Renamed the project from `axumapi` to `siderite` (crates, rust paths, CLI binary, config file `siderite.toml`, env prefix `SIDERITE_`, migration history table `siderite_migrations`, docs site). Axum remains the HTTP engine.
- CLI serve command is `run` (was `runserver`). The `siderite` binary wraps `cargo run` in an application package and adds `siderite new`.

### Added
- **Backends/Cache:** explicit `tls` feature enables SQLx and Redis Rustls
  transport. Deployments still configure certificate/identity verification.
- **Backends:** PostgreSQL/MySQL `connect_with_init` composes an explicit
  SQLx callback before mandatory session initialization on every fresh
  connection. `connect_with` now documents stored callback replacement.
- **Benchmarks:** concurrent insert correctness batches verify exact HTTP
  201, unique returned IDs and committed values after each timed trial.
  One independent batch query checks sample rows; failures reject the trial
  and retain evidence. Samples remain outside performance measurements.
- **MySQL:** experimental `mysql-native` feature exposes
  `mysql::native::NativeMySqlBackend` and `NativeMySqlOptions`. Clean ORM
  sessions retain prepared statements; cancelled exchanges, raw SQL and
  unfinished transactions retire their connections. Admission is bounded,
  checkout has a deadline, and connection startup warms every pool slot.
  SQLx remains the default; TLS/recovery/migration acceptance remains open.
- **Benchmarks:** a pinned native MySQL driver probe compares SQLx against
  reset/retained mysql_async pools and held connections, with independent
  complete ID/row validation and live cancellation/reuse prerequisites.
- **Core:** `ManagedLifespan`, `App::run_until` and `shutdown_timeout` provide
  supervised resource cleanup and a shared shutdown deadline (30 seconds by
  default). Unix SIGTERM requests shutdown alongside Ctrl-C.
- `siderite build` runs `cargo build` in the app package and forwards its arguments, e.g. `siderite build --release`.
- GitHub Pages documentation site (`website/`, Astro Starlight) covering getting started, tutorials, HTTP/data/production guides, reference, internals, and contributing. Deployed from `.github/workflows/pages.yml` with rustdoc at `/api/`.

### Fixed
- **Lifespan:** startup/bind failures unwind initialized resources; caller
  cancellation and last test-client drop retain cleanup ownership. Hook
  panics and timeouts do not skip remaining cleanup attempts. Original
  startup/bind errors take priority over cleanup errors.
- **ORM:** cancelled nested savepoints and interrupted statements now abort
  the transaction instead of allowing their writes to commit. Failed
  savepoints are rolled back and released; cleanup failure preserves the
  original error and invalidates the transaction. Escaped handles are closed.
- **Backends:** compile shared pool helpers only for backends that use them;
  strict lint passes for each backend feature independently. CI checks the
  seven minimal feature combinations across all test targets, avoiding
  all-features unification. SQLite-only integration tests are feature-gated.
- **Benchmarks:** reject failed or incomplete load-generator trials and verify
  committed insert counts. Trials use isolated application lifecycles,
  schema-preserving resets, alternating pairs and duration calibration;
  retain raw evidence and manifests. Smoke runs are explicitly labelled.
- **Benchmarks:** require actual keep-alive negotiation when requested;
  reject mismatched transports instead of comparing their throughput.
  MySQL fixtures and FastAPI decode URL credentials consistently and share
  session setup and matched-row update semantics with the Rust adapters.
- **Migrations:** SQLite table rebuilds no longer CASCADE-delete child rows. The migrator turns `PRAGMA foreign_keys` off on the dedicated connection before `BEGIN` (SQLite ignores that pragma inside a transaction), runs `PRAGMA foreign_key_check` before commit, and restores the previous value afterwards.
- **Migrations:** `migrate` and `rollback` take a backend lock (PostgreSQL `pg_advisory_lock`, MySQL `GET_LOCK`, SQLite `BEGIN IMMEDIATE`) and re-read history under it so concurrent replicas cannot double-apply.
- **Migrations:** a MySQL migration that fails after earlier DDL has committed reports `MigrationError::MysqlPartial` with the 1-based statement index, because MySQL cannot roll the earlier statements back.
- **Migrations:** `RenameHints::rename_model` emits `RenameModel` (and `ALTER TABLE … RENAME TO` when the table name changes). An unhinted delete+create of a same-shaped table, or remove+add of a same-shaped column, is refused so data is not dropped. **Breaking:** `diff` / `diff_with` return `Result`.
- **Migrations:** `RunRust` operations run in the declared order among SQL operations, including on reverse.
- **Migrations:** lock-held PostgreSQL/MySQL migration connections are closed instead of returned to the pool, so a session lock that was not released can never be handed to another caller.
- **Migrations:** the SQLite pre-commit `foreign_key_check` fails only on *new* violations and is skipped when foreign keys were already off, so pre-existing violations in unrelated tables no longer block every migration. The baseline is diffed row by row (child table, rowid, parent), so a migration that repairs one violation while adding another fails instead of passing on an unchanged count.
- **Migrations:** documented that SQLite `RunRust` data code runs with foreign keys off and must stay FK-clean by hand; the commit error names the offending table and row.
- **Migrations:** documented that a SQLite `migrate`/`rollback` run is a single all-or-nothing transaction and removed the now-dead per-migration SQLite branches in the executor.
- **Migrations:** MySQL `GET_LOCK` now waits indefinitely for the migration lock, matching PostgreSQL `pg_advisory_lock`, so a slow migration no longer makes other replicas time out and crash-loop.
- **Migrations:** MySQL records per-statement progress (`siderite_migration_progress`) and resumes a failed migration at the first statement that did not commit, including inside one operation (a `CreateModel` with indexes), instead of replaying committed statements; a `RunRust` failure after committed statements or after another `RunRust` wrote data reports `MigrationError::MysqlOpPartial` naming the operation.
- **Migrations:** `MysqlPartial` is raised only when earlier DDL actually committed (first-keyword check), so a failure after non-DDL statements reports the plain error instead of a misleading partial.
- **Migrations:** a PostgreSQL migration with `"atomic": false` (the setting `CREATE INDEX CONCURRENTLY` needs) runs outside a transaction, so a mid-way failure left its earlier statements committed and a re-run replayed them (`already exists`). It now records per-statement progress in `siderite_migration_progress` and resumes at the first statement that did not commit, like MySQL, in both directions; a failure after committed statements reports `MigrationError::PostgresPartial` with the statement index, or `MigrationError::PostgresOpPartial` for a `RunRust`. Atomic PostgreSQL migrations and SQLite still record no progress.
- **Migrations:** `RenameHints::allow_drop_model` / `allow_drop_field` approve an intentional drop that collides with a same-shaped create, so the refusal no longer forces a two-migration split. The shape predicate stays conservative on purpose: weakening it would silently drop data on genuine renames.
- **Migrations:** `RenameModel` retargets other models' foreign keys that pointed at the old table, so hand-written renames leave project state consistent.
- **Migrations:** `RenameModel` also retargets the renamed model's own self-referencing foreign keys (a `parent_id` tree), which previously kept pointing at the old table name.
- **Migrations:** the MySQL progress row stores the migration checksum, and a resume refuses when the file changed after the partial run instead of skipping statements by stale indices. The crash window between a committed statement and its progress write is documented with manual recovery steps.
- **ORM:** the default `Backend::begin_schema` returns an error for `transactional = false` instead of silently returning a transaction. A SQLite non-transactional schema change that fails `foreign_key_check` now says the changes are already committed.

### Changed
- **Breaking (migrations):** SQLite `migrate` / `rollback` run as one transaction and refuse to run on a `Db` that is already in a transaction; the per-migration `atomic` flag is ignored there, so `VACUUM` and similar statements cannot appear in SQLite migrations.
- **Migrations:** `Report.sql` is rendered under the migration lock from the re-read plan, so a concurrent replica applying migrations in between no longer desyncs it from `Report.planned`.
- **Migrations:** PostgreSQL `migrate` / `rollback` no longer fail with `unsupported PostgreSQL type VOID`. `SELECT pg_advisory_lock($1)` returns `void`, which SQLx cannot decode, so acquiring the migration lock always errored; it is now wrapped in a subquery returning a decodable column. Releasing checks the returned `bool` and reports a lock that was not held.
- **RouteCache is opt-in.** It stores only responses marked `Cache-Control: public` (new `Cached::public(ttl, response)` helper), or every response of a route whose layer sets `RouteCache::default_ttl`. Entry lifetime follows `s-maxage`/`max-age`, and `no-cache` responses are no longer stored. New `MethodRouter::layer` applies middleware to a single route. `Cached` is in the prelude, and the `todo_sqlite` example caches `GET /todos`. **Breaking:** `RouteCache::new` takes only the cache; use `.default_ttl(ttl)` for the old store-everything behaviour.
- **Extractors:** `Option<T>` is `None` only when the input is absent (a missing header, query string or credential). Present but invalid input now fails with `T`'s error instead of becoming `None`. `ApiError::absent()` / `is_absent()` mark and detect such errors. **Breaking:** `Option<Security<..>>` with an invalid token returns `401` instead of treating the request as anonymous, and custom extractors must mark their missing-input error with `.absent()` to keep returning `None`.
- Security review fixes:
  - **Body:** `Body::into_bytes` stops at `DEFAULT_BODY_LIMIT` (2 MiB) instead of buffering without limit; `Body::into_bytes_limited` sets another cap. `BodyError::is_too_large` reports the overflow and `?` into `ApiError` returns a `413` problem response. **Breaking:** callers reading larger bodies must use `into_bytes_limited`.
  - **Config:** secret detection in error messages matches key substrings and suffixes (`database_url`, `access_token`, `private-key`, `smtp_pass`, `*_dsn`, ...), and out-of-range integer errors are redacted too.
  - **Validation:** `#[field(url)]` now rejects values that are not absolute URLs (`url_parsing`). **Breaking:** `Constraint` has a new `Url` variant.
  - **RouteCache:** the key includes scheme and `Host` and is length-prefixed, so virtual hosts sharing a cache stay isolated and split header values cannot collide. Headers whose names look like credentials (`X-Auth-Token`, `X-Session-Id`, ...) bypass the cache by default; other credential headers still need `bypass_header`.
  - **Observability:** the docs routes (`/openapi.json`, `/docs`, `/redoc`) record their matched route in `http.request` spans instead of `<unmatched>`.
- Phase 6 review fixes:
  - **RouteCache:** requests carrying `X-API-Key`, `Proxy-Authorization` or a header registered with `bypass_header` skip the cache. Keys include `Accept` and `Accept-Encoding`, and responses that `Vary` on other headers are not cached. Streaming bodies and bodies above `max_body_bytes` are never buffered. Cached `HEAD` responses keep the original `Content-Length`.
  - **Security:** the schemes of one handler form a single requirement object (all are required); `Option<Scheme>` adds an anonymous alternative.
  - **ORM:** subquery plans remember their database, and `Db` rejects queries and bulk writes containing a subquery from another database.
  - **dbshell:** a `?password=` URL parameter is moved out of `psql`'s arguments, and `sslpassword` is refused.
  - **CLI:** `AppCli::configure_db` and `AppCli::database_router` hooks; `blog_postgres` attaches its receivers there instead of on every request.
  - **Config:** `set()` overrides win regardless of call order.

### Added
- Examples `blog_postgres`, `todo_mongo` and `polyglot`, and the configuration and cache guides (`docs/CONFIG.md`, `docs/CACHE.md`).
- Phase 6 production tooling:
  - **Configuration:** `siderite-config` layers defaults, TOML, environment and overrides with figment. It supports per-alias database URLs and redacts `Secret` values. `init_tracing` installs a subscriber.
  - **Observability:** `http.request` spans record request id, method, matched route, status and latency. `orm.query` spans record query durations. Neither records bind parameters, headers or query strings.
  - **Security:** HTTP Bearer, HTTP Basic, API key (header, query or cookie) and OAuth2 password flow extractors document themselves in OpenAPI `securitySchemes`. `Security<T, S>` handles authentication and scopes, and `ApiError` can carry response headers.
  - **Signals:** `pre_save`, `post_save`, `pre_delete`, `post_delete` and `m2m_changed`, with explicit registration and `#[receiver]`.
  - **Database routing:** the `DatabaseRouter` trait, `Databases::{for_read, for_write, objects, using}` and `App::database`. Querysets bound to different databases cannot be combined.
  - **Cache:** the `siderite-cache` crate with an in-memory LRU cache (with TTL), a Redis cache and `RouteCache` middleware.
  - **CLI:** `AppCli` provides `run`, `routes`, `check`, `dbshell` and the migration commands. `CliSettings` can be built from `Settings`. The standalone binary connects to PostgreSQL and MySQL, and there is a MySQL schema editor.
  - **Testkit:** `TestDatabase` provides in-memory SQLite with models or migrations and rolled-back isolation. `TestClient::builder` adds DI overrides.
  - **Benchmarks:** Criterion benchmarks, with measured medians in `crates/siderite-bench/README.md`.
  - **Release tooling:** a CI workflow (lint, test, live databases, MSRV 1.92, cargo-deny), `docker-compose.yml` and the release guide.
- Phase 5 backends: MySQL 8 adapter (emulated `RETURNING`, dialect-aware compiler), MongoDB executor compiling the supported QuerySet subset to filters and aggregation pipelines (unsupported relational features return capability errors), and a typed Redis key/hash/set client. Live tests run when `MYSQL_URL`, `MONGODB_URL` or `REDIS_URL` is set.
- Phase 4 ORM: `#[derive(Model)]` with typed field constants, `QuerySet` (filter/order/annotate/aggregate/windows/set operations), `ForeignKey` / `OneToOne` / many-to-many, `select_related` / `prefetch_related`, instance `save` / `delete` / `refresh`, transactions and savepoints. `ForeignKey` implements `Validate`, `Schema` and `Dump` by delegating to the related primary key. PostgreSQL executor (`PgBackend`) and Django-style JSON migrations (autodetector, schema editor, CLI `migrate` / `rollback` / `showmigrations` / `squashmigrations`). Example: `todo_sqlite`.
- Phase 3 validation and serialization: type-driven `prepare` → Serde → `validate` pipeline reporting every error with its location; `#[derive(Validate)]`, `#[model_config]`, shared `#[field]` constraints; `#[model_hooks]` with field/model validators (before/after), computed fields and serializers; `Dump`/`DumpOptions`/`JsonDump`; extractors validate automatically (422); constrained URL, IP, UUID, decimal, float, integer and list types; validation guide and Pydantic equivalence table.
- Phase 2 HTTP framework: siderite-owned `Handler`/extractor/response traits with OpenAPI `describe` hooks; OpenAPI 3.1 generation (components/$ref reuse, validated against the official schema) with Swagger UI and ReDoc; route attribute macros, `routes![]` and `#[derive(Schema)]`; dependency injection (`Depends`, request-scoped caching, overrides, cycle detection, teardown); middleware (CORS, compression, trusted hosts, HTTPS redirect, request id, logging, timeout, concurrency, body and rate limits) with documented ordering; lifespan hooks and resources; forms, multipart, typed headers, cookies, redirects, streaming and file responses, WebSockets, background tasks, static files.
- Phase 1 foundation: workspace, QueryPlan IR, typed expressions, backend capabilities, SQL compiler (PostgreSQL/SQLite), SQLite executor, core HTTP app, validation primitives, testkit.
