# Changelog

## [Unreleased]

- Own HTTP/HTTP2 and WebSocket work through shutdown, with bounded transport
  admission and readiness phases; overdue cooperative workers are aborted
  and joined before resource teardown.


### Added
- **CLI:** `siderite generate resource NAME [FIELD:TYPE[?]]...` writes
  a model and `POST /TABLE` / `GET /TABLE/{id}` handlers with validated
  request bodies (422), 404 for missing rows and `State<Databases>`
  access; tested against SQLite. Unauthenticated examples; no migration
  is run.
- **CLI:** `siderite generate model NAME [FIELD:TYPE[?]]... [--table T]`
  writes a `Model` with an `id` primary key and scalar fields (`i32`,
  `i64`, `f64`, `bool`, `String`, nullable with `?`); reserved and
  unsupported names and types are refused. It is tested through
  `makemigrations` and a SQLite round trip.
- **CLI:** `siderite generate route NAME [--path /URL] [--dry-run]`
  writes `src/routes/NAME.rs` with a documented handler, an explicit
  operation ID and a `routes()` function, and prints the registration
  lines instead of editing the app. The output is compiled in the test
  suite against the current API.
- **CLI:** `siderite add` / `siderite remove` run `cargo add` /
  `cargo remove` for the selected app package, so Cargo owns manifest
  edits (workspace inheritance and sources are kept). `add --preset
  postgres|mysql` enables a backend by adding the feature to the existing
  `siderite-cli` dependency (`--dry-run` previews it); `remove --preset`
  describes the manual edit instead of guessing.
- **CLI:** `siderite ai validate [--json] [--live]` reports the
  `verify` run as bounded findings with locations, `explain` fixes,
  documentation links and reproduce commands; it applies no fixes and
  states that passing is not proof of correctness. `verify`'s `check`
  step now carries the framework diagnostics (`framework_diagnostics`).
- **CLI:** `siderite ai context --task TEXT [--budget BYTES]
  [--include PATH]...` selects the matching models, routes, docs sections
  and compiled examples, plus explicitly included project files (inside
  the package, git ignore rules honored, secret-looking files refused),
  within a byte budget with deterministic truncation. Each part names its
  source and version; omissions and missing information are listed.
- **CLI:** `siderite explain CODE [--json]` and `explain --list`: the
  cause, minimal fix and verification command of every siderite
  diagnostic code, for this version. Compiler codes and Clippy lints point
  to their official pages; unknown codes exit `1` (`explain.E001`).
- **CLI:** `siderite docs search QUERY [--json] [--limit N]` searches
  a documentation index packaged with the CLI (website guides, reference,
  tutorials and the compiled examples) offline. Results carry the docs
  version, source file and line, a link at the release tag and the Cargo
  features a section names; a project locking another siderite version
  gets a `docs.W001` warning.
- **CLI:** `siderite verify [--json] [--live]` runs `fmt --check`,
  Clippy, the build, the tests and the framework checks with one
  aggregate report (per-step status, duration and redacted failure
  output). A failed build skips the steps that need it; `--live` adds the
  ignored tests. Files are never reformatted and no migration is applied.
  Compiler diagnostics keep their codes, spans and rendered explanations
  (`diagnostic_format` 1, bounded and deduplicated); test outcomes are
  reported per step (`test_results: "aggregate"`) on stable Rust.
- **CLI:** `siderite completions bash|zsh|fish` prints completion scripts
  derived from the command contract (commands, subcommands and flags);
  shell profiles are never edited.
- **CLI:** `siderite setup [--json] [--install]` checks rustup, `rustc`
  against siderite's `rust-version`, `cargo`, `rustfmt`, `clippy` and a C
  linker, with the exact fix per operating system. `--install` runs only
  the needed `rustup` commands; shell profiles and system packages are
  never changed.
- **CLI:** a command contract (`siderite_cli::commands`): each command
  declares where it runs, what it needs, what it may change, live access
  and output formats. Help texts and dispatch are derived from it, and
  `siderite commands [--json]` lists it (versioned) for AI clients.
- **CLI:** `ai init [--check] [--dry-run] [--force]` writes
  `.siderite/context.md`: a generated block (commands, conventions, the
  project's models and routes, and examples compiled in the test suite
  for this release) between markers that record the format, framework
  version and a hash. Reruns report up to date, stale (with a diff) or
  hand-edited (left alone without `--force`); text outside the markers is
  always kept, and a file without markers is never touched.
- **CLI:** `new --dry-run` prints the exact files without writing them.
  File-writing commands share one write plan that rejects absolute and
  `..` paths, writes through symbolic links and in-plan collisions, never
  overwrites existing files (identical ones are left alone, others stop
  the plan before any write) and writes each file atomically. Package
  names that are Rust keywords are rejected.
- **CLI:** `ai dump-context [--format markdown|json] [--live]
  [--max-bytes N]`: the application snapshot as Markdown or a JSON
  envelope for AI tools, offline by default with migration status
  labelled `unknown`; `--live` reads it read-only and bounded, and a
  failure is a redacted `ai.E001`. The context names its binary and build
  time, and stays the only stdout output when run through `siderite`.
- **CLI:** `siderite_cli::report::snapshot`: a bounded, versioned
  application snapshot (framework version, compiled features, database
  aliases with backend kinds only, models, documented routes and the
  redacted OpenAPI document) with per-section completeness markers and
  deterministic truncation under configurable limits.
- **CLI:** `siderite dev` watches the app's inputs: the source files in
  the binary's dep-info (path dependencies included), manifests,
  `Cargo.lock`, `siderite.toml` and `migrations/`, never the target or
  VCS directories. Bursts are debounced, watcher overflows count as
  changes, and migrations are never applied automatically. Uses `notify`
  8.2 (CC0-1.0).
- **CLI:** `siderite dev [--grace-ms N] [-- ARGS]` supervises the app: it
  builds with structured cargo output, runs the built binary, keeps the
  last healthy process on a failed build, and replaces it on success after
  a graceful stop (SIGTERM to the process group, SIGKILL after the grace
  period, waiting until the group is gone). Rebuild requests during a
  build coalesce; `r` + Enter rebuilds; Ctrl-C cancels the build and stops
  the app without leaving processes behind.
- **CLI:** `fmt`, `lint` and `clean` passthroughs: `cargo fmt`, `cargo
  clippy` and `cargo clean` in the app package with arguments forwarded
  unchanged and the exit status preserved. `lint` is Clippy, distinct from
  the framework `check`; a missing `rustfmt`/`clippy` component exits 1
  with the `rustup component add` command.
- **CLI:** `doctor --live [--database ALIAS]... [--timeout-ms N]`:
  read-only probes of the configured databases (connect plus migration
  status), at most four at a time and each within a timeout (default
  5 s). Only requested aliases can fail, errors are redacted, SQLite is
  opened read-only and never created, and Redis/MongoDB are not probed.
- **CLI:** `siderite doctor [--json]`: offline checks of `rustc`/`cargo`
  against siderite's and the project's `rust-version`, `rustfmt` and
  `clippy`, configuration loading, database URL schemes against the
  backends compiled into the binary, and (advisory) the listen address.
  Missing native database clients are informational. Each problem has a
  code and a fix hint; nothing connects, builds or writes.
- **CLI:** `showmigrations --json`, in the app binary and the standalone
  binary: the selected alias, the URL source (never the URL), and every
  migration's `id` / `applied` flag in text order, from the same
  `Migrator::show()` rows as the text. The read is read-only and bounded
  to 30 seconds; failures are envelopes, never an empty success.
  `siderite_migrations::cli::render_status` renders the text form.
- **CLI:** `openapi export [--output PATH] [--force]` writes the app's
  OpenAPI 3.1 document (the same `App::openapi()` that serves
  `/openapi.json`, pretty-printed) from the app factory alone: no database
  connection, server or migration, and it works with HTTP docs disabled.
  File output is atomic and never replaces an existing file without
  `--force`; stdout output stays clean of factory prints.
- **CLI:** `check --json` and `routes --json` (the flag may come before or
  after the command) write one versioned JSON envelope to stdout with the
  usual exit codes. Failures, usage errors included, are envelopes with
  `data: null` and a `cli.E001`-`cli.E003` diagnostic. `siderite` runs the
  app with a private report file (`SIDERITE_REPORT_PATH`) and sends the
  app's stdout to stderr, so factory prints cannot corrupt the JSON.
- **CLI:** typed reports and an output contract (`siderite_cli::report`):
  a versioned JSON envelope (`schema_version`, `command`, `ok`, `data`,
  `diagnostics`) with a JSON Schema in `docs/schemas/cli-report-v1.json`,
  pure builders for `check`, routes and registered models, text and JSON
  renderers, and a shared redaction helper. `check` and `routes` now render
  through them with unchanged text output and exit codes. No `--json` flag
  yet. `siderite_config::is_secret_key` is now public and also treats
  `pwd`, `sig` and `signature` keys as secret. Redaction covers passwords
  with unencoded `/ ? # @`, spaces or quotes, comma-separated URL lists and
  `key=value` secrets outside URLs (libpq DSNs, `;Pwd=`, `sig=`).
  `Diagnostic` fields are private (use its accessors) and are redacted
  again when serialized or rendered.

### Changed
- **Migrations:** `Migrator::lock_holder()` and `LockHolder` name the
  session holding the migration lock (PostgreSQL `pg_locks` with
  `pg_stat_activity`, MySQL `IS_USED_LOCK` and the process list).
  `migrate` / `rollback` print it after `LockTimeout`; the error variant is
  unchanged and successful runs do not query it. `LockReleaseUncertain`
  now says the release could not be confirmed and that the ids were
  applied or unapplied by the run. Documented when each engine frees the
  lock of a dead holder.
- **Migrations:** `RecoveryEntry` gains `total_operations` (the migration's
  operation count in the supplied graph) and `all_operations_recorded()`,
  so `inspectmigrations` shows when only the history write is outstanding.
  The history-write recovery procedure now requires that check and an
  empty `uncertain_steps` before a re-run, and says that a hand-edited
  history must also drop the migration's progress row.
- **CLI:** explicit project selection. `siderite` discovers the app with
  `cargo metadata --no-deps` and accepts `--manifest-path`, `--package` /
  `-p` and `--bin` before the command (arguments after the command are
  forwarded unchanged). Nested packages, virtual workspaces and several
  binaries resolve deterministically; ambiguity exits 2 with the
  candidates, and a missing `cargo` exits 1 with an install hint. Plain
  single-binary packages skip `cargo metadata`; standalone commands still
  work under a broken or virtual `Cargo.toml`.
- **Migrations:** live-tested the migration lock on PostgreSQL and MySQL:
  bounded wait, cancelled waiters and holders, server-side kill of the lock
  holder, and roles without CREATE or bookkeeping-table write access. A run
  that finishes but cannot confirm lock release (for example the session
  was lost) now fails with the new `MigrationError::LockReleaseUncertain`,
  listing the migrations that did run, instead of a bare driver error. The
  change is on the error path only; successful runs are unchanged.
- **Migrations:** documented and live-tested the crash boundaries between
  DDL execution and progress recording on PostgreSQL and MySQL (kill before
  a script, after DDL, after progress, after intent clear); no runtime change.
- **Migrations:** live-tested kills immediately before and after the history
  row insert and delete (apply and rollback) on PostgreSQL atomic and
  non-atomic and MySQL; documented how `inspectmigrations` distinguishes
  recorded success, rollback and uncertainty, with operator procedures;
  no runtime change.
- **Cache:** the route-cache metadata bound is enforced. Each namespace
  admits at most `RouteCache::max_new_targets` (default
  `DEFAULT_MAX_NEW_TARGETS`, 10,000) new target markers per
  `generation_ttl` window, counted exactly. Markers are created lazily when
  a response is first stored, so uncacheable responses never use budget;
  once the window budget is spent, new targets bypass with
  `x-cache: miss` and the layer remembers the spent window, so such
  requests make one backend read. Admission is one backend call through the
  new provided method `Cache::admit_marker` (Redis: one `EVALSHA` script
  whose window comes from the server `TIME`, so client clock skew cannot
  loosen the bound; `MemoryCache`: one lock), and a write is one call
  through the new provided method `Cache::invalidate_marker` (Redis
  `MULTI`/`EXEC`). `Cache::increment_with_expiry` is the new atomic
  counter primitive behind the generic fallback. Fills that begin without
  a marker are guarded by 64 striped epoch keys per namespace: a missing
  or changed epoch refuses the fill (a write refuses only first fills in
  its stripe), and the epoch is created by the first request that needs it.
  The epoch key format (`route-v2-epoch:<namespace>` plus a stripe
  number) is new in this release, so no compatibility handling exists.
  The admission script calls `redis.replicate_commands()` when present
  (Redis 3.2 to 6 without effects replication).
  Documented bound: at most `2 x max_new_targets` markers and
  `3 x max_new_targets x variants` representations per namespace.
  Generation tokens are now a per-layer random seed (64 fully random
  bits) plus a counter (unique, not secret), so a write draws no random
  bytes. Build the layer after forking in prefork servers, since forked
  copies share the seed and counter. Third-party `Cache`
  adapters need no change: both new methods have defaults built from
  `get`, `set`, `delete`, `set_if_absent_with_ttl` and
  `increment_with_expiry`. An adapter lacking either of the last two
  makes route caching stop storing (one warning) before any budget is
  spent and without leaving a marker. The generic sequence is not atomic
  and takes its window from each process's clock; override
  `admit_marker`/`invalidate_marker` for the store's atomic path.
  Wrappers that count calls see an extra epoch read per first request.
  `MemoryCache::stored_bytes` is new. Redis scripts now run through
  `redis::Script` (`EVALSHA`), which enables the `redis` crate's `script`
  feature (adds `sha1_smol`). The hit path is unchanged. Redis eviction
  settings are never modified.
- **Cache:** route-cache generation markers are now finite. They are
  created with `set_if_absent_with_ttl` and the new
  `RouteCache::generation_ttl` (default one hour,
  `DEFAULT_GENERATION_TTL`); representation TTLs are clamped to
  `min(response ttl, generation_ttl)`; invalidation deletes the marker
  instead of writing a non-expiring one. Custom `Cache` adapters that
  return `CacheError::Unsupported` for `set_if_absent_with_ttl` now bypass
  route caching (`x-cache: miss`) instead of getting non-expiring markers
  (one warning is logged and the state lasts for the life of the layer).
  `generation_ttl(Duration::ZERO)` disables route caching without backend
  calls. A `max-age` or `default_ttl` above `generation_ttl` is effectively
  capped to it. Upgrading with a persistent Redis keeps old non-expiring
  markers until a write to that target deletes them (or the namespace
  changes).
- **Cache:** `Cache::set_if_absent_with_ttl` adds atomic expiring
  insertion (zero TTL rejected, sub-millisecond TTL rounded up) with an
  explicit `CacheError::Unsupported` default for external adapters;
  implemented for `MemoryCache`, `Arc` and `RedisCache` (a single
  `SET NX PX`; TTLs above `i64::MAX / 2` ms are rejected, not clamped).
  Generation expiry, metadata bound and bypass contract documented in
  `docs/CACHE.md`.
- **SQLite:** writers of one `SqliteBackend` (statements, raw writes,
  scripts, transactions and transactional schema changes) queue in FIFO
  order for one write slot instead of racing for the file lock and sleeping
  in SQLite's busy handler; reads keep the whole pool. A write that cannot
  get the slot within `with_write_timeout` (five seconds by default) fails
  with "database is locked", including one issued outside a transaction the
  same task holds open.
- **SQLite:** opt-in `SqliteBackend::group_commit` lets concurrent
  autocommit writes share one transaction and commit sync, each in its own
  savepoint and acknowledged only after the commit.
- **Benchmarks:** SQLite comparisons use the same pool size in both apps.
  FastAPI's new default `pooled` strategy keeps `--pool-size` sqlite3
  connections used from worker threads; single-connection strategies
  require `--pool-size 1`. `--siderite-group-commit` is reported as a
  separately labelled workload.
- JSON responses stream ordinary derived models and containers directly,
  avoiding an intermediate JSON tree while preserving dump hooks, custom
  implementations, field exclusions, and numeric conversion semantics.
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
