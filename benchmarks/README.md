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

`--pool-size N` sets the same pool size in both applications, SQLite
included. FastAPI's default SQLite strategy (`--fastapi-sqlite-mode pooled`)
keeps N blocking sqlite3 connections used from worker threads. The
single-connection strategies (`pooled-sync`, `per-request`) require
`--pool-size 1`, so Siderite also runs one connection.
Pool 10 with concurrency 20 remains an intentional saturation workload.

SQLite defaults use rollback journal and `synchronous=FULL`.
`--sqlite-wal` selects WAL and `synchronous=NORMAL` in both applications;
report this as a separate durability profile.
`--siderite-group-commit` enables Siderite's opt-in commit coalescing for
concurrent autocommit writes; because FastAPI has no counterpart and commits
per request, report this as a separately labelled workload.

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
that implementation is labeled and needs an async worker/mixed-load profile
before broad production performance claims.

SQLite defaults to `--fastapi-sqlite-mode pooled-sync`: one cached native
sqlite3 connection, statement autocommit and explicit lifespan cleanup.
Handlers do not await while borrowing it. This is a strong synchronous
insert-throughput baseline, while database work still blocks unrelated loop
activity. `--fastapi-sqlite-mode per-request` opens/closes a private
connection per request and is reported separately. Both enforce foreign
keys and the named synchronous policy. Earlier per-request results used
implicit BEGIN/explicit COMMIT and did not close connections explicitly;
they cannot be combined with either current profile.


Latency gates account for quantization. New trials retain ApacheBench's
percentile CSV (three decimal places in milliseconds); older console values
have one millisecond resolution. Each p99 comparison uses the conservative
upper bound `(S + resolution_S / 2) / (F - resolution_F / 2)`. A nonpositive
baseline lower bound cannot pass. The CSV parser checks all 101 percentiles,
ordering, finite values and exact precision; missing or invalid files reject
a trial. See the [ApacheBench source](https://github.com/apache/httpd/blob/2.4.x/support/ab.c)
for console and CSV formatting. Precision is not measurement accuracy.


SQL startup logs now sample effective session and durability settings and
retain a sanitized `database-settings.jsonl` beside runtime records. Capture
uses fixed queries and validates the owned PID and setting-key whitelist.
It does not certify every physical connection, storage guarantees under
faults or equal CPU budgets. Compare these records before combining runs.


AB CSV indexing is unsafe for very small sample sets. The runner requires
at least 100 requests and explicit warm-up requests; default warm-up is at
least 100. Invalid CSV remains retained and prevents trial publication.

Build benchmark provenance with `python3 benchmarks/build_benchmarks.py
--features mysql-native` (on one shell line). A `.build.json` sidecar records
the exact binary digest, stable source digests, command, toolchain and SQLite
link configuration. The run manifest reports whether current inputs match.
For a same-engine SQLite experiment, point `PKG_CONFIG_PATH` at the installed
Python SQLite library's pkg-config directory and set
`LIBSQLITE3_SYS_USE_PKG_CONFIG=1` only for that build. Verify startup versions;
never assume the system default pkg-config selects Python's engine. This is
a local build record, not hermetic or independently attested provenance.
