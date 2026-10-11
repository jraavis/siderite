# Siderite vs. FastAPI Benchmarks

This directory contains the end-to-end HTTP benchmark suite comparing Siderite
against FastAPI, matching the methodology described in
[`docs/BENCHMARKS.md`](../docs/BENCHMARKS.md).

## Quick Start

### 1. Prerequisites

- **ApacheBench (`ab`)**: Available by default on macOS (`/usr/sbin/ab`) or via
`apache2-utils` on Linux.
- **Rust Toolchain**: `cargo` with edition 2024.
- **Python 3.12+**: With dependencies installed:
  ```bash
  pip install -r benchmarks/fastapi/requirements.txt
  ```

### 2. Build Siderite in Release Mode

```bash
cargo build --release -p hello_world -p siderite_todo
```

### 3. Run the Benchmarks

To run the **Plain HTTP** suite (matching `examples/hello_world`):
```bash
python3 benchmarks/run_benchmarks.py --suite plain
```

To run a fast sanity check (lower request counts):
```bash
python3 benchmarks/run_benchmarks.py --suite plain --fast
```

To run the **Database-backed Todo API** suite (e.g. SQLite):
```bash
python3 benchmarks/run_benchmarks.py --suite db --db sqlite
```

To run against PostgreSQL (ensure Docker container is running):
```bash
DATABASE_URL=postgres://siderite:siderite@127.0.0.1:55432/siderite \
  python3 benchmarks/run_benchmarks.py --suite db --db postgres
```

## Workloads

The plain suite measures `GET /`, `GET /hello/world`, and `POST /echo`.
The database suite measures `GET /todos` (20 rows), `GET /todos/1`, and
`POST /todos` (one independently committed row per response).

Use a dedicated benchmark database. The runner creates `todos` before
opening application pools. Each trial starts only its measured application,
warms it, resets rows without recreating the schema, measures, and shuts
down before starting the next application. Trial order alternates S/F then
F/S. Each database reset restores 100 initial rows outside timed work.

`--pool-size N` controls the configured pools. SQLite FastAPI still uses
blocking sqlite3 connections opened per request; its lifecycle differs from
Siderite's pool and must be identified in published comparisons.
Pool 10 with concurrency 20 remains an intentional saturation workload.

SQLite defaults use rollback journal and `synchronous=FULL`.
`--sqlite-wal` selects WAL and `synchronous=NORMAL` in both applications;
report this as a separate durability profile.

## Validity and evidence

The runner defaults to five paired repetitions. Normal runs calibrate a
shared request count to the faster application's observed rate, with a
minimum measured duration of ten seconds. `--min-seconds N` changes this
minimum; `--timeout N` bounds each load-generator process. `--fast` skips
calibration and uses reduced counts for smoke checks only.

Trials with process failures, timeouts, missing metrics, incomplete counts,
transport errors, non-2xx responses, or insufficient duration abort the
comparison. Invalid trials are retained and never enter normal medians.

For database inserts, each trial first requires exactly HTTP 201, validates
its returned values, and reads the committed row through an independent
client. After warm-up and reset, the measured row-count increase must equal
the requested count. Each valid timed insert trial is followed by a separate
concurrent correctness batch at the same concurrency (capped at 100). Every
sample must return exactly HTTP 201, correct values and a unique positive ID.
One independent database query checks all sampled IDs and stored values.
Failures reject the trial; response and row evidence is retained in
`response_checks`. Sample writes happen after the measured row-count check
and never enter timed throughput or latency metrics. ApacheBench still
cannot prove the exact status or ID of every measured response; the retained
sample evidence explicitly records that limitation.

Evidence is saved under `benchmarks/logs/trials-*`: raw load-generator
output, validation reasons, completed counts, row deltas, application logs,
and a manifest of runner settings, source revision, release binary hashes,
platform, Python and installed Python driver versions. Hashes identify
binaries but do not prove which source built them. Database versions,
effective settings, storage, and build flags still require recording.

Short smoke results are unsuitable for performance claims. Published
comparisons additionally need the durability/transaction matrix, sampling,
full-pool warm-up verification and paired confidence intervals described in
[plan 3](../docs/INSERT_OPTIMIZATION_PLAN_3.md).

## Runner regression checks

```bash
python3 -m unittest discover -s benchmarks -p 'test_*.py'
```


## Native MySQL driver diagnostics

[mysql_driver_probe](mysql_driver_probe/README.md) compares SQLx with pinned
mysql_async using the same prepared INSERT, independent autocommit,
connection count and session setup. It verifies every returned ID and row
through an independent connection. Default-reset and retained-session modes
are separate profiles; held connections isolate checkout/release cost.

This driver experiment predates the experimental native ORM adapter. It is
not a FastAPI comparison. Its live cancellation/reuse contract participates
in the workspace's ignored live-test suite.

## Experimental native MySQL HTTP profile

Build the benchmark with the opt-in adapter, then select it explicitly:

```bash
cargo build --release -p siderite_todo --features mysql-native
python3 benchmarks/run_benchmarks.py --suite db --db mysql \
  --mysql-driver native --todo-operation insert --runs 5 --min-seconds 10
```

Provide `MYSQL_URL` for a dedicated disposable database. `--mysql-driver
sqlx` selects the compatibility control. `--todo-operation` supports `all`,
`insert` and `reads`. Both SQL MySQL profiles warm the full configured pool;
the native adapter warms again after schema setup retires a raw connection.

Fresh connections are the default transport. Requested keep-alive now
requires ApacheBench's actual keep-alive count to match every completed
request. A local HTTP/1.0 run negotiated keep-alive with Siderite but none
with FastAPI/Uvicorn; that asymmetric run is excluded from performance
evidence. A valid persistent-connection comparison needs a generator and
protocol profile that both servers actually support.

Native HTTP measurements remain diagnostic until exact response status/ID
sampling, server-log review, effective database settings and build provenance
meet plan 3's publication gates. A driver-probe gain is not an HTTP gain.


The runner records repetition identities in timed trial JSON and emits a
paired-comparison record for each workload. It reports the geometric mean
of within-pair Siderite/FastAPI throughput ratios and a deterministic 10,000
resample 95% percentile interval (seed 0). It resamples whole pairs rather
than treating concurrent requests as independent observations. Fewer than
five pairs produce no interval. The lead gate requires at least five
identified pairs, every timed trial lasting at least ten seconds, and a
lower bound strictly exceeding 1.10x. Both adoption and strong-lead gates
also require each pair's p99 ratio <=1.05 and equal observed scheduler
worker counts. Plan 3 adoption instead requires a point ratio >=1.10 and a
confidence lower bound above parity. Zero-rounded baseline tail latency
cannot establish a ratio. Failure to establish a lead remains
visible alongside valid trial results; smoke runs cannot pass this gate.
These intervals do not remove systematic bias or generalize to other hosts,
database settings, workloads or deployment architectures.


SQLite durability profiles are separate: `--sqlite-profile default`,
`--sqlite-profile wal-full` and `--sqlite-profile wal-normal`. WAL/FULL keeps
synchronous=FULL on each application connection. WAL/NORMAL selects NORMAL,
which has a weaker acknowledgement durability policy. The legacy
`--sqlite-wal` flag remains an alias for WAL/NORMAL and cannot be combined
with the explicit profile. Output labels and manifests preserve the choice.


CLI database runs create a unique `siderite_bench_<uuid>` database on the
configured server, or a temporary SQLite file. They remove only that owned
fixture after all application processes stop, including trial failure and
interrupt handling. PostgreSQL/MySQL credentials need database creation and
removal rights; the configured application database is not reset. MongoDB
clients follow the owned URL database while retaining its original
`authSource`. Failed cleanup reports the generated name for reconciliation.
Direct `run_todo_suite` calls must supply their own disposable database/file.

Runtime/load controls: `--concurrency`, `--requests`, `--warmup-requests`
and `--runtime-workers`. Rust defaults to one scheduler worker; FastAPI
runs one Uvicorn process/event loop. Larger Rust worker counts are retained
as unmatched experiments and cannot pass the comparison gates. Startup
reports verify actual Rust worker counts against the requested setting,
validate the owned PID and retain sanitized records in `runtime.jsonl`.
Additional driver/blocking workers and database resources are not equivalent
CPU budgets. Initial request counts can be raised by normal calibration.

The plain FastAPI baseline now uses async handlers for nonblocking work;
previous thread-pool-handler results must not be combined with it. SQLite's
FastAPI baseline still performs blocking SQLite calls in its event loop;
that implementation must be labeled and supplemented with a modern pooled
worker baseline before broad production performance claims.

## Inspecting interrupted work

`inspectmigrations` emits a read-only JSON snapshot of applied ids and
recorded partial work. It does not create history/progress tables, acquire
the migration lock, change schema or delete recovery rows. Database
connection initialization still follows the chosen adapter policy.

```text
siderite inspectmigrations --database-url URL --migrations-dir migrations
siderite migrate --lock-timeout-ms 30000
```

The report includes direction, completed operations/statements, checksum,
current-file match (`null` for an unknown migration) and history status.
Catalog/table permission failures and malformed bookkeeping are errors.
Reads may race with another migrator; inspect while migration writers are
stopped for a stable operator diagnosis.

PostgreSQL/MySQL advisory acquisition defaults to 30 seconds. Set
`Migrator::with_lock_timeout` or `--lock-timeout-ms` to change it. Timeout
returns `MigrationError::LockTimeout`; retry after the holder finishes.
This limit does not bound pool acquisition, SQLite BEGIN or migration
execution. Session release is checked, including MySQL lock ownership.

Observed lock behaviour (live PostgreSQL and MySQL, `tests/lock_live.rs`):

- A waiter gives up with `LockTimeout` within its bound while another
  session holds the lock, writes nothing and leaves the holder's ownership
  intact. The message does not name the holder.
- Aborting a waiting migrate future leaves no lock or pending request; the
  lock is free once the holder releases it.
- Dropping a migrator mid-run closes its session. The server only notices
  once the statement in flight finishes, so the lock is freed after that
  statement ends, not instantly. A long-running step delays other migrators.
- If the holder's session is killed (`pg_terminate_backend`, MySQL `KILL`)
  during a step, the server frees the lock at once, the run fails with a
  driver error, nothing is recorded, and the next migrator proceeds.
- If the session is lost after the work finished but before release, the
  run returns `MigrationError::LockReleaseUncertain` naming the migrations
  that did run. They are recorded; re-running is a no-op. Check
  `inspectmigrations` before assuming either outcome.
- A role without CREATE, or without write access to the history/progress
  tables, fails with the server's permission-denied error and the lock is
  released. PostgreSQL rolls an atomic migration back; MySQL DDL commits
  implicitly, so a table may exist without a history row (inspect and
  reconcile). Pool acquisition is not covered by the lock bound.

Owned process-kill checks verify the boundary between executing DDL and
recording progress. A child process migrates through a test-only backend
wrapper that aborts at an exact point; the migrator carries no crash hooks.
Bookkeeping is not proof of schema state. See "Crash boundaries" below for
the observed outcome and operator procedure per engine.

Arbitrary non-transactional RunRust callbacks may have committed data before
failure; inspect their effects and establish replay safety explicitly.
The current inspection command does not certify exactly-once data work.



### Crash boundaries

`tests/crash_boundaries_live.rs` kills the migrating process (`abort`) at
deterministic points and then inspects, retries and repairs, against live
PostgreSQL and MySQL. Two operations are used: generated `CreateModel` DDL
(no intent) and explicit `RunSQL` scripts (intent recorded). Observed results:

| Kill point | PostgreSQL `atomic: true` | PostgreSQL `atomic: false` / MySQL |
| --- | --- | --- |
| Script intent written, script not yet run (`RunSQL`) | No intent is ever written (atomic runs are not resumable); the kill rolls back, inspection is empty, plain retry applies | Intent only, no schema change. Retry fails `UncertainSqlStep` |
| DDL committed, progress not written (first op) | Rolled back; schema, progress and history empty; plain retry applies | Table exists; inspection shows no partial entry. Retry fails loudly ("already exists"); `RunSQL` fails `UncertainSqlStep` first |
| DDL committed, progress not written (later op) | Rolled back as above; no progress row, so earlier ops are not `partial` | Inspection shows the earlier ops as `partial`; retry fails loudly as above |
| Progress written (`RunSQL`: intent not yet cleared) | Not applicable (no progress or intent) | Progress shows the statement complete. Generated DDL resumes with a plain retry; `RunSQL` still fails `UncertainSqlStep` |
| Intent cleared after progress | Not applicable | No uncertainty; plain retry resumes without replay |
| Rollback (`rollback`) killed after its first DDL (generated) | Not tested (same transaction rollback as apply) | Table dropped, history row and progress unchanged. Retry fails loudly (table missing) and changes no bookkeeping; recreate the dropped object, then roll back again |

Rules the tests enforce:

- A retry never silently replays DDL or scripts whose completion is
  unrecorded. It stops with `UncertainSqlStep` or the database's own
  "already exists" error, and does not run later operations; the tests
  assert the error kind and that inspection output is unchanged afterwards.
- A changed migration file is refused while recorded progress or an intent
  exists (`changed since a partial ...`); inspection shows
  `file_matches: false`. The checksum safeguard does not apply to the
  generated-DDL "DDL committed, progress not written (first op)" window,
  where nothing was recorded yet, which is why that case needs catalog
  reconciliation.
- PostgreSQL atomic migrations are all-or-nothing; the history row commits
  with the schema, so there is nothing to reconcile.
- MySQL and PostgreSQL non-atomic runs cannot be made exactly-once: DDL
  commits first and progress is a separate write.

Operator procedure for an unrecorded committed statement: stop migration
writers; inspect with `inspectmigrations`; compare the catalog with the
migration file; drop the object the unrecorded statement created (or advance
the progress row by hand); for `RunSQL`, delete the matching row from
`siderite_migration_intents` only after the schema and data are verified;
then re-run `migrate`, which resumes at the first unfinished operation.
The tests exercise the drop-and-retry form of this procedure, and the
delete-intent-then-resume form when progress already records the statement.

#### History-write boundaries

`tests/crash_history_live.rs` kills the process immediately before and after
the history row is inserted (apply) or deleted (rollback), using generated
two-operation `CreateModel` migrations on live PostgreSQL and MySQL. All DDL
has finished at these points; only the history write is in question.
`inspectmigrations` (`inspect_recovery`) tells the cases apart by comparing
`applied` with the `partial` entry's `direction` and `history_applied`.

| Kill point | PostgreSQL `atomic: true` | PostgreSQL `atomic: false` / MySQL |
| --- | --- | --- |
| Before history insert | Rolled back: no tables, no history, no progress. Plain retry applies | Tables exist; `applied` empty; one `apply` entry at all operations, `history_applied: false` (uncertain: only the catalog proves the DDL ran). Retry succeeds, writes the history row, replays no DDL. A changed file is refused (`changed since a partial apply`) |
| After history insert | Rolled back as above (the insert was uncommitted) | Recorded success: `applied` lists the id and the `apply` entry has `history_applied: true`. Retry succeeds as a no-op and leaves the stale progress row. A changed file is refused (`checksum mismatch`). Repair: delete the stale `apply` progress row |
| Before history delete (rollback) | Rolled back: tables and history intact; plain rollback works | Tables dropped, history row present; `unapply` entry with `history_applied: true`. Retrying `rollback` resumes, deletes the history row and replays no DDL |
| After history delete (rollback) | Rolled back as above | Recorded rollback: `applied` empty, `unapply` entry with `history_applied: false`. Retrying `rollback` is a no-op and leaves the stale row; the next `migrate` clears it, or delete it by hand |

A stale `apply` row after a recorded success is harmless: the next
`rollback` replaces it with `unapply` progress. Neither stale direction
caused a skipped or replayed operation in the cycle tests. Procedure: stop
migration writers; run `inspectmigrations`; for `history_applied: true` on
`apply` (or `false` on `unapply`) confirm the schema, then optionally delete
the row from `siderite_migration_progress`; otherwise re-run the same
command. No intent or schema reconciliation is needed because every
operation is recorded as complete before the history write.

### Unconfirmed explicit work

For MySQL and non-atomic PostgreSQL migrations, the executor records an
intent before each explicit `RunSQL` script or registered `RunRust` callback.
It clears the intent after successful execution and progress recording.
`inspectmigrations` includes these in `uncertain_steps`; `callback: null`
identifies a SQL script. Scripts may commit several commands before failing.
A remaining intent blocks automatic retry with `UncertainSqlStep` or
`UncertainRustStep`, even if the failure appeared harmless. This deliberately
changes automatic retry behavior for explicit non-transactional scripts.

Stop migration writers, preserve the original checksummed file, and inspect
the actual schema, data and external effects before reconciliation. Preserve
the intent until that investigation establishes a safe next operation.
Deleting an intent blindly may duplicate committed data. There is currently
no automatic repair or acknowledgement command; a verified repair requires
an operator-controlled bookkeeping change. Generated DDL still has a
commit-to-progress crash window that requires catalog reconciliation.

A callback can be registered with `register_replay_safe` when its complete
behavior is idempotent, including side effects outside the database. The
framework treats this as a caller declaration, not an exactly-once guarantee.
Ordinary `register` removes a previous replay-safety declaration when replacing
that name. SQLite and atomic PostgreSQL use transaction rollback instead.

Migration execution requires a pool `Db`. Passing a transaction or dedicated
connection handle fails before migration bookkeeping or DDL, preserving the
caller's transaction ownership. Read-only inspection accepts those handles.


If a replay-safe callback already recorded completed-operation progress,
but termination interrupted intent deletion, recovery clears that guarded
intent and skips the completed callback. Forward and reverse recovery tests
verify the callback is not run again. An ordinary registration still fails
the uncertainty guard; completion records do not automatically authorize
replay or repair for arbitrary callbacks/scripts.
