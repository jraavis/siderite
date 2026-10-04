# Performance and framework closure progress

The active objective includes SQL/NoSQL rewrites where they improve measured
performance, plus closure of the framework gaps. Completion requires the
acceptance gates in [plan 3](INSERT_OPTIMIZATION_PLAN_3.md) and the
[gap closure plan](FRAMEWORK_GAP_CLOSURE_PLAN.md). Historical forecasts in
[plan 2](INSERT_OPTIMIZATION_PLAN_2.md) remain unverified.

## SQLite write gate and group commit checkpoint: 2026-10-04

Implemented in-process FIFO write serialization (`WriteGate`) and opt-in group commit (`GroupCommit`) on `SqliteBackend`:
- Under concurrent pool writes (10 connections), uncoordinated writers previously collided on SQLite's file lock, causing connection threads to sleep in SQLite's busy handler backoff (up to 100 ms) and inflating p99 latency to ~152 ms (insert throughput: ~1,258 req/s, 0.64x vs FastAPI).
- In-process `WriteGate` coordinates write slot acquisition across clones without busy sleeping. Default rollback journal insert throughput rose to **2,715 req/s** (**2.00x** speedup) with p99 tail latency dropping from 152 ms to **5.39 ms**.
- Opt-in `--siderite-group-commit` batches concurrent autocommit writes into a single shared transaction and commit sync, boosting insert throughput to **8,589 req/s** (**5.59x** speedup) at **3.69 ms** p99.
- WAL mode (`--sqlite-wal`) with non-blocking concurrent reads and writes reached **14,508 req/s** (**3.31x** speedup) at **1.85 ms** p99.

## Response rewrite checkpoint: 2026-10-04

Replaced ordinary JSON response tree construction with direct encoding.
The same-executable old-algorithm control measured 773.25 ns against
361.80 ns for the new path: 53.2% less time. Forty targeted tests and strict
release library clippy passed. Another workspace build was present; these
are diagnostic microbenchmarks, not isolated HTTP/database release gates.
See [response rewrite evidence][response-rewrite].

[response-rewrite]:
  ../benchmarks/evidence/response-serialization-20261004/README.md

## Database and correctness checkpoint: 2026-10-03

The implementation includes an opt-in native MySQL adapter, shared MySQL
execution, transaction ownership, supervised background work, transport
shutdown/readiness, bounded middleware/cache admission, proxy/authority
policy, prefetch bind budgets, composed SQLx session initialization and
verified TLS across network adapters. Later sections contain exact evidence.

The performance objective remains open. Current controlled insert results
use five alternating pairs, observed one-worker runtimes, effective SQL
settings and conservative fine-resolution tail gates:

| Backend/profile | Paired Siderite/FastAPI ratio | 95% interval | Gates |
|---|---:|---:|---|
| SQLite 3.53.4 WAL/FULL, one lease | 1.574x | 1.501–1.664x | Both pass |
| MySQL native, ten connections | 1.012x | 0.965–1.063x | Both fail |
| MySQL SQLx, ten connections | 0.923x | 0.894–0.959x | Both fail |
| PostgreSQL SQLx, ten connections | 1.231x | 1.179–1.286x | Both pass |
| MongoDB, ten connections | 1.217x | 1.160–1.264x | Both pass |

These describe separate experiments, not a directly paired native/SQLx
comparison. Earlier native MySQL 1.127x results lacked observed matching
workers and cannot establish adoption. Earlier SQLite 1.678x results lacked
matched-engine evidence. The newer runs above take precedence.

The full workspace check passed 920 tests with 95 explicit ignores. Eighteen
selected live PostgreSQL/MySQL migration checks passed, including committed
SQL/callback crashes, lock deadlines and caller-transaction preservation.
Forty-nine Python benchmark checks passed. Current public behavior includes
pending-step recovery intents and read-only recovery inspection.

Additional workloads,
physical-pool coverage, CPU/storage budgets, remaining crash boundaries and
final release checks remain open. G19 additionally records unbounded Redis
route-generation retention and its required expiry/admission work. Evidence
and limitations are recorded in the sections below.

## Implemented: benchmark validity foundation

- Reject failed or timed-out load generators, missing metrics, incomplete
  counts, transport errors, non-2xx responses and short normal trials.
- Preserve raw evidence and invalid reasons before computing summaries.
- Alternate paired application order, with five repetitions by default.
- Run only one application per trial; warm and shut down each fresh process.
- Create schema before application startup and reset only data during trials.
- Validate insert preflight status/body and independently read its stored row.
- Require measured insert row delta to equal the requested request count.
- Calibrate equal request counts for both apps to a ten-second minimum.
- Record source/binary identity, Python versions, options and server logs.
- Split validation, fixture, database-check and process code into modules.

Validation on 2026-10-02:

- Fourteen runner regression tests passed.
- Python lint checks passed with a 79-character limit.
- Current release binaries built successfully offline.
- Plain HTTP and SQLite default-profile smoke suites passed with two pairs.
  SQLite WAL-profile smoke checks also passed with one pair. Insert
  preflight and committed-row checks passed for both applications.
- Plain HTTP calibration passed with a shortened 0.1-second check minimum.
  These runs verify harness behavior, not publication-ready performance.

At this initial checkpoint, no driver replacement or production throughput
improvement was established; later native MySQL evidence is recorded below.

## Remaining benchmark acceptance work

- Verify warm-up coverage of every pool connection and equal eager opening.
- Extend correctness sampling across the full database/workload matrix and
  validate structured server logs.
- Extend sampled SQL settings and local build provenance to full physical
  pool coverage, storage/CPU budgets and the remaining NoSQL profiles.
- Controls and paired confidence bounds are implemented; complete the
  fairness/provenance matrix using actual runtime and session observations.
- Verify reset lifecycle, cancellation, and count checks on PostgreSQL,
  MySQL and MongoDB with isolated databases.
- Establish the full workload/durability matrix and component profiles.

## Remaining engine and framework work

At the initial checkpoint all performance phases beyond the harness
foundation remained open: ORM
allocation work, SQLite writer serialization, MySQL/PostgreSQL release-cost
experiments, bounded template caching, optional batching/native drivers,
and MongoDB key allocation. Preserve cancellation, generated-key, trigger,
transaction and durability guarantees during every experiment.

## Implemented: transaction ownership and feature lint

G01/G02 implementation now uses explicit connection/scope ownership.
Cancelled scopes and interrupted statements make the whole transaction
unusable. Outer success cannot commit abandoned child writes. Parent and
sibling operations are rejected while a child owns the savepoint stack;
true recursive nesting remains supported. Failed savepoints are rolled
back and released. Cleanup failure retains the original closure error and
invalidates the transaction. Closed/escaped handles cannot execute work or
register commit hooks. Discarded hooks cannot retain their own scoped
handle through a reference cycle. Commit-outcome ambiguity is documented.

Evidence on 2026-10-02:

- Six deterministic scope scenarios passed on SQLite, PostgreSQL 17.11 and
  MySQL 8.4.11, with one-connection pools enforcing safe connection reuse.
- Five fault/cancellation tests cover SAVEPOINT, RELEASE, ROLLBACK, a pending
  statement, original-error preservation and dedicated schema connections.
- ORM/backend default-feature test suites passed (eight doc examples ignored).
- Strict workspace all-feature lint and the workspace all-feature test
  suite passed. These runs exclude explicitly ignored live tests; the SQL
  scope contracts were also run separately against all three live engines.
- G11: strict backend library lint passed with no features and each single
  feature (SQLite, PostgreSQL, MySQL, MongoDB, Redis). CI includes this matrix.

The PostgreSQL/MySQL tests create and drop uniquely named isolated databases.
Explicit live tests fail if service URLs or database creation rights are
missing; they do not silently skip database work.

G04 resource ownership is implemented through a lifespan supervisor.
Eight regression scenarios passed for startup/bind failure, hook factory
panic, startup/shutdown cancellation, last-client drop, deadline handling
and explicit termination during startup. Cleanup reverses initialization
and preserves the primary failure. Partial initializer work remains owned
by the initializer; runtime destruction cannot guarantee async cleanup.

G05 is partial: custom termination, Unix SIGTERM handling and a shared
HTTP-drain/teardown deadline are implemented. Real signal subprocess tests,
readiness/admission transitions and forced ownership of HTTP, streaming,
WebSocket and background tasks remain required.

At this historical checkpoint G03, G06-G10 and G12-G18 remained open.
The next priority was background-task
ownership and complete shutdown validation. Full framework release
validation and measured
performance acceptance remain required; focused tests cannot replace them.


## Native rewrite scope

Plan 3 section 10 makes specialized SQL adapter prototypes an explicit
workstream under the user's rewrite authorization. MySQL, SQLite and
PostgreSQL are evaluated independently. Native adapters and corresponding
production throughput gains remain unimplemented/unmeasured. MongoDB and
Redis require
cost-specific optimization rather than mandatory driver substitution.


## Native MySQL diagnostic implementation

Added `benchmarks/mysql_driver_probe`, pinned to mysql_async 0.37.1 and
SQLx 0.8.6. It compares the same prepared, independently committed INSERT
with matched pools/session setup, full-pool warming, paired ordering,
per-insert latency and independent complete ID/row checks. Retained-session
and default-reset native profiles are distinct; held mode diagnoses pool
checkout/release separately. At this experiment's checkpoint, no native
adapter was exposed; the implementation update below supersedes that state.

Evidence on 2026-10-02:

- The live one-slot native reuse contract passed for both native modes:
  typed transaction-drop rollback, cancelled query cleanup, cancelled
  checkout capacity, session reset/retention and session setup restoration.
- The complete three-mode probe passed uneven worker allocation and queueing
  with independent row validation. Missing live service configuration fails.
- Strict probe lint, Rust 1.92 compatibility and dependency policy passed.
- Five paired pooled passes each inserted 50,000 rows per mode on local
  MySQL 8.4.11, concurrency/pool 10, flush-on-commit 1 and sync-binlog 1.
  All 750,000 acknowledged IDs/rows were verified; every trial exceeded 10 s.
- Native retained-session paired throughput was 1.204x SQLx, with a paired
  bootstrap interval of 1.140x-1.280x. Median p99 was 5.409 ms versus
  6.627 ms for SQLx. Native reset with required session restoration was
  slower (paired 0.681x). These are driver-only local results, not a FastAPI
  comparison, a deployment promise or proof of full adapter correctness.

The result supports implementing an opt-in native MySQL adapter with explicit
lease/session ownership. It does not justify accepting arbitrary session
mutations into a reset-disabled pool. RETURNING, all values, triggers,
transactions, migration locks, restart/TLS and unknown commits remain gates.
Raw trials, summary and source/binary/environment provenance are retained in
`benchmarks/evidence/native-mysql-20261002/`.


Five paired held-connection passes then verified another 900,000 rows.
Retained native/SQLx paired throughput was 1.009x (interval 0.965x-1.056x),
so execution on held connections did not demonstrate a meaningful lead.
Together the pooled and held controls support removing recycling overhead
through explicit connection ownership, rather than promising a universal
native-driver speedup. See the
[native MySQL experiment][native-experiment]
for raw data, methodology, environment and adoption limits.


[native-experiment]:
  ../benchmarks/evidence/native-mysql-20261002/README.md


After the native experiment, workspace formatting, strict all-targets /
all-features lint and all-features tests passed. Explicitly ignored live
suites are excluded from that workspace run; the native MySQL contract was
run separately. A server query confirmed no disposable probe schemas remain.

## Shared MySQL engine and feature isolation

The SQLx MySQL adapter now uses four private modules for driver operations,
SQLx value conversion, cached metadata and RETURNING reconstruction. The
shared engine accepts a statically dispatched driver interface. Public
SQLx constructors remain unchanged. The original MySQL source is below
1,000 lines, and the extracted
modules satisfy the 1,000-line/79-character limits.

MySQL RETURNING preparation compiles the original borrowed write plan
without cloning and clearing it. The compiler still rejects unsupported
capabilities and malformed statements; the public MySQL compiler still
rejects native RETURNING. Four regression tests cover equivalent statement
text and bind values, default values, malformed rows and unsupported nested
query capabilities. No performance gain is claimed for this change without
a paired measurement.

Focused validation passed: 72 backend unit tests, strict backend lint with
all features/all targets, and all-feature backend rustdoc. The shared engine
also passed 17 live MySQL tests and the six-scenario MySQL transaction scope
contract. These include generated/supplied keys, triggers, stored values,
transaction isolation and cancellation-safe connection reuse.

G11 follow-up found that library-only feature checks missed SQLite-specific
integration imports during MySQL-only all-target builds. SQLite test entry
points and the shared fixture are now feature-gated; CI checks all targets
for each individual backend and no features. This strengthens feature
isolation without suppressing lint failures. Combined profiles and minimal
downstream rustdoc/MSRV coverage remain acceptance work.

The six isolated all-target lint profiles passed locally: no features,
SQLite, PostgreSQL, MySQL, MongoDB and Redis. Strict all-target/all-feature
workspace lint and formatting also passed after these changes.
The full all-feature workspace test run completed with 887 passed, zero
failed and 23 explicitly ignored. This is the ordinary workspace run, not
an all-database live run; the MySQL live contracts above were exercised
separately with an actual service URL.

At this checkpoint, native value conversion and lease ownership were next.
The implementation below now covers those prototype tasks. Full migration
recovery, restart/TLS and unknown-commit fault injection remain release gates.
The native diagnostic's earlier 1.204x pooled result does not measure this
refactor or the full framework against FastAPI.

## Experimental native MySQL adapter

The opt-in `mysql-native` feature now exposes `NativeMySqlBackend` and
`NativeMySqlOptions`. It reuses the common RETURNING engine through static
driver dispatch and implements canonical values, pinned transactions and
schema scopes. Defaults are 10 connections, 100 waiting callers and a
10-second acquisition deadline. Startup warms every slot. Clean ORM sessions
retain prepared statements; raw SQL clears metadata and retires its session,
while unfinished transactions and cancelled exchanges retire their sockets.
No failed write or ambiguous commit is automatically retried.

The live ownership contract caught an important retention error: a native
pool minimum of zero discarded clean connections even with reset disabled.
Minimum and maximum are now equal, and the regression checks physical
connection identity rather than accepting correct rows alone. Additional
checks cover admission bounds, cancelled waiters, escaped interrupted
transactions, dropped transactions, killed connections, dirty session state,
schema locks, decode errors and errors in later stored-procedure results.
Backend debug output omits driver options and credentials.

The prototype passed 17 live native ORM tests and the six-scenario native
transaction-scope contract. The updated ownership contract passed after
pool retention and full warming were corrected. Focused unit tests cover
canonical decoding, date/time boundaries, admission limits and credential
redaction. Earlier 887-test evidence predates this adapter; final verification
is recorded separately below.

The HTTP runner explicitly selects native or SQLx and shares full-pool
warming and MySQL session/update semantics with FastAPI. Actual keep-alive
negotiation is now validated. An HTTP/1.0 smoke run retained connections only
for Siderite, so its apparent advantage is excluded. Fresh-connection paired
insert diagnostics completed against both adapters. These do not yet meet
publication gates for exact response sampling, logs and full provenance.

Five paired native/FastAPI passes measured a 1.127x geometric throughput
ratio (bootstrap interval 1.073x-1.184x), with median p99 12/15 ms. The SQLx
control series measured 0.863x (0.745x-0.935x), with median p99 16/13 ms.
All 20 timed row deltas matched completed requests, totalling 1,151,490
verified inserts. The native lower confidence bound is below 1.10x, so the
proposed reliable 10% minimum lead is not established. The two driver series
were sequential, not directly paired with each other. One slow SQLx trial
was retained. See [raw HTTP evidence][native-http] for limitations and logs.

[native-http]:
  ../benchmarks/evidence/native-mysql-http-20261003/README.md

SQLx remains the default. TLS, server restart, complete migration recovery,
unknown-commit faults and the full workload matrix remain acceptance work.
SQLite/PostgreSQL prototypes and remaining framework gaps are still open.

The live SQLx and native control reruns each passed all 17 MySQL ORM tests
and all six scenarios in `mysql_scope_contract`. The native ownership
contract also passed again. The first combined scope invocation required an
unset PostgreSQL URL and failed its PostgreSQL setup; the corrected targeted
MySQL invocations passed. This is not an all-database live run.

The shared live RETURNING/storage tests now have their own module. The
MySQL test entry file is 957 lines and its RETURNING module is 331 lines;
both satisfy the 79-character width rule, as do the native implementation
and new fixture modules. This closes these touched files' size violation,
not the repository-wide G18 baseline.

Final checks passed: strict all-feature/all-target workspace lint,
warning-free all-feature workspace rustdoc, formatting, all seven isolated
backend lint profiles and 16 benchmark runner regression tests. Rust 1.92
builds the native-only backend profile across all targets. Dependency
advisory/bans/license/source checks passed with existing duplicate and
unused-license-allowance warnings. Native-only all-target lint and formatting
passed again after extracting and formatting the shared live tests.

The all-feature workspace test run finished with 893 passed, zero failed
and 24 explicitly ignored. It does not replace the live service checks
above. The final live MySQL runs exercised the extracted test modules for
both drivers, including the low-privilege trigger read-back case.

## Implemented: concurrent insert response correctness sampling

Each valid timed insert trial now runs a separate concurrent correctness
batch after the full measured row-count check. The batch uses the load
concurrency, capped at 100, and requires exact HTTP 201, correct JSON values,
unique positive integer IDs, and matching committed rows. All sampled rows
are read in one independent parameterized database query. Evidence records
individual status/ID checks, verified rows and explicit untimed scope;
failures invalidate and retain the trial before any summary is computed.
Preflight reuses the same bounded response parser.

Validation on 2026-10-03:

- Runner regression checks cover malformed/oversized bodies, wrong 2xx
  status, boolean/invalid IDs, duplicate IDs, missing or incorrect stored
  rows, bounded sample concurrency and retained invalid-trial evidence.
- Twenty-four runner regression tests and Python lint checks passed; all
  changed Python modules meet the 1,000-line and 79-character limits.
- Live one-pair insert smoke checks passed for SQLite, PostgreSQL, SQLx
  MySQL and native MySQL against FastAPI: eight timed trials, 4,000 measured
  committed inserts and 80 separately verified sample responses/rows.
  SQLite used a temporary file; MySQL/PostgreSQL used unique databases that
  were removed after the checks. MongoDB remains unverified live.
- Retained raw smoke evidence and scope notes are in
  [response correctness evidence](
  ../benchmarks/evidence/response-correctness-20261003/README.md).
  Reduced workloads establish correctness, not a throughput advantage.

These samples do not prove the status or returned ID of each ApacheBench
response. Full publication still requires database settings/build evidence,
structured log validation, the workload matrix and repeated confidence gates.

## Implemented: G03 bounded request-owned background tasks

Queued work now requires a successful endpoint response. Request guards
release admission and discard futures on cancellation, panic, extraction
failure and handler error. Explicit route status overrides apply before
execution is authorized. Default instances are inert; custom extraction
wrappers propagate a background-ownership flag. Non-background handlers
avoid request-queue allocation.

Each app has bounded task admission, bounded active batches and tracked
workers. Batches preserve sequential execution, isolate task panic and carry
request tracing context. Lifespan shutdown closes admission and drains
accepted work before resource teardown within the same deadline, aborting
remaining workers on expiry. Weak request-to-manager ownership prevents
accepted futures from keeping their own supervisor alive in a cycle.
Mounted apps have scoped queues within the parent lifespan.

Streaming acceptance means endpoint response production, not confirmed
body delivery. Middleware can still replace a produced response. Crash
persistence, blocking task cancellation, HTTP connection ownership and
upgraded WebSocket drain remain separate acceptance work. The API and
operational limits are documented in [background tasks](BACKGROUND_TASKS.md).

## Implemented: G06 bounds and G07 peer/authority policy

Rate limits use normalized typed IP keys, configurable hard entry capacity,
conservative new-identity denial at saturation and a maximum of 16 expiry
candidates per admission. Depleted existing clients are retained. Finite
positive refill and burst configuration are validated. Four deterministic
unit checks include 100,000 identities, hard bounds and incremental expiry.

`TrustedProxies` is shared by rate-limit client identity and HTTPS scheme
resolution. Exact trusted peers must replace headers; untrusted fields are
ignored and malformed/duplicate trusted fields fail explicitly. HTTPS
redirect default now ignores forwarded scheme. Typed host validation,
IPv6 brackets, canonical destination ports and allow-lists are supported.
Nine HTTP policy tests and the existing 20 middleware tests passed.
Cache context now shares the resolved proxy policy. Real proxy deployment
coverage remains open.

Background task lifecycle verification: twelve targeted tests passed, plus
the existing normal-completion/task-panic checks. Workspace strict lint
passed after the background changes; later middleware changes require final
workspace revalidation.

## Implemented: G08 cache validity and G09 byte admission

Refresh, no-store, conditional/range bypass, origin Date/Age and hop header
removal now follow the documented caching subset. Successful unsafe writes
rotate shared random target generations across GET/HEAD/header variants;
delayed fills and evicted markers cannot resurrect old representations.
Explicit related-target invalidation supports application dependencies.
Namespaces isolate independent layers; stable namespaces intentionally
share data and generations. Failed invalidation disables that layer.

MemoryCache bounds both entry count and retained key/value bytes, with
size-aware LRU eviction, checked TTLs and admission that preserves old
values on oversized replacement/increment. Route keys, headers, bodies and
versioned base64 encoding have separate byte limits. A 1 MiB binary fixture
encodes below 1.4 MB without cloning the full body before encoding.

Validation: twelve HTTP contracts, six codec/key tests and twenty-two
memory checks passed. Atomic generation creation and cross-layer write
invalidation also passed against live Redis using an isolated UUID prefix.
Strict workspace all-target/all-feature lint passed with these changes.
Final workspace tests/docs remain required. Cache/database invalidation is
not distributed atomic; partition/restart policy needs bounded TTLs and
application coordination. Allocation/mutex profiling remains open.

## Implemented: G10 bounded admission and optional body permits

ConcurrencyLimit now bounds execution and queued requests, applies a queue
wait deadline and returns 503 with Retry-After when admission is unavailable.
Clones share permits/counters; cancellation releases both execution and
waiting capacity. Optional body ownership retains permits through stream
completion/error/drop without buffering. Closing admission wakes waiters;
configuration validates semaphore capacity and representable deadlines.
Queue snapshots and completed wait timing are available through `stats()`.
Middleware order determines whether Timeout includes admission waiting.

Eight targeted admission/body tests and the existing middleware suite cover
these policies. Upgraded connection budgets, server socket ownership and
forced streaming shutdown remain G05 work. Default queue bounds are a
behavior change; applications should configure the queue before cloning.

## Implemented: G13 prefetch key and bind budgets

Prefetch now hash-deduplicates keys after one canonical conversion each.
The explicit target handle supplies capabilities and exact compiled base
bind counts. Existing filters reduce batch capacity, NULL literals consume
no slots and unknown/exhausted SQL budgets fail before target queries.
Repeated keys share loaded target objects. Empty target querysets skip I/O;
sliced querysets spanning batches fail rather than repeat limits/offsets.
SQL adapters expose compiler counts through Backend/Db diagnostics.
Nine existing relation tests and four bounded-target regressions exercise
these contracts. Large-key scaling/allocation profiling remains open.

## Implemented: G12 explicit SQLx initialization composition

PostgreSQL/MySQL `connect_with_init` accepts a shared custom callback, runs
it on every new session and applies mandatory settings afterwards. Existing
`connect_with` documents replacement of callbacks stored privately by SQLx;
other pool options remain effective. External pool initialization and reset
remain the owner's contract.

Live verification passed on PostgreSQL 17.11 and MySQL 8.4.11. Tests checked
two physical sessions, replacement after terminating one test-owned session,
custom markers, required UTC/SQL-mode/GROUP_CONCAT settings and initialization
failure. Services were not restarted and no application tables were changed.
The two ignored tests require explicit service configuration and run in the
existing all-feature live CI job.

## Security guidance and verified transport support

The recommended security guide now uses authorization code with PKCE and
application-owned trusted access-token verification. It covers signature,
issuer/audience, token type/lifetime/scopes, state/nonce, key refresh and
storage/CSRF. Legacy password compatibility is labeled explicitly. Provider
negative integration tests remain outside framework extraction validation.

G15 review found TLS was not compiled for SQLx/Redis. The explicit `tls`
feature enables their Rustls transport; selecting a verifying connection
mode remains deployment configuration. Positive and negative live contracts
passed for PostgreSQL, SQLx/native MySQL, Redis and MongoDB against owned
TLS fixtures. Each rejects a wrong trust root and hostname. The fixture
runner removes its services and ephemeral keys on success and failure.
The dated RSA dependency exception exposure review is recorded in
[dependency exceptions](DEPENDENCY_EXCEPTIONS.md); its removal still requires
an upstream fix or dependency replacement. TLS benchmarks and production
certificate rotation remain separate validation.


## Implemented: G05 transport ownership and readiness

The server owns accepted HTTP sockets and HTTP/2 protocol workers. Socket
and stream admission have explicit hard bounds. Pending/established
WebSocket callbacks reserve bounded app-wide admission and run under the
lifespan supervisor. The engine handoff retains only a channel, so it cannot
retain or execute an abandoned user callback after supervisor shutdown.

Shutdown stops listening, marks readiness Draining, drains owned work and
aborts/joins overdue cooperative tasks before resource teardown. Caller
cancellation also aborts HTTP work; the cleanup supervisor waits for drops.
Readiness exposes Starting, Ready, Draining and Stopped for application probes.
Blocking code and runtime/process destruction remain outside async guarantees.

Real socket tests passed for held HTTP/1 handlers, pending response streams,
HTTP/2 streams, WebSockets, upgrade overload, idle socket admission/recovery,
normal keep-alive drain and serving-future cancellation. A real SIGTERM
subprocess proved cancellation occurs before teardown and the listener closes.
Readiness transitions and actual SIGINT delivery also passed. Final
workspace release checks and refreshed throughput measurements remain open.


## Implemented: paired confidence reporting

Timed trial records now carry their repetition identity. The runner checks
matched counts, concurrency, transport and identities, then resamples whole
pairs to report geometric throughput ratios and 95% percentile bounds.
Seed/draw count, per-pair ratios, durations, lead threshold and gate reasons
are retained per workload. Reports display paired bounds rather than a
ratio of unrelated medians. Short or unidentified trials and fewer than
five pairs cannot satisfy the lead gate. Twenty-nine Python regression
checks passed, including variability that defeats an apparent median lead,
repeated/mismatched identities and the exact 1.10x boundary. These are
harness checks; refreshed release benchmarks remain required.


## Implemented: explicit SQLite durability profiles

The harness supports default, WAL/FULL and WAL/NORMAL as separately labeled
profiles. Both applications configure the same per-connection sync setting;
the legacy WAL flag remains a clearly documented WAL/NORMAL alias. Six
release smoke trials passed complete committed-row checks and response
sampling across the three profiles: 3,000 timed rows and 60 sampled inserts.
Evidence is retained in
`benchmarks/evidence/transport-sqlite-profiles-20261003/`. Each paired record
fails the publication/lead gate because these trials are short and have
only one pair. Effective application PRAGMA readback, durability fault tests
and full performance acceptance remain required.


## Implemented: G17 explicit live mode and owned benchmark fixtures

PostgreSQL, MySQL, MongoDB and Redis integration tests are explicitly ignored
in offline runs. Selecting them without the required URL now fails rather
than counting an early return as success. Four missing-configuration checks
proved this behavior. Seventy-five selected live checks passed: 50 backend
contracts, two SQLx initialization checks, five Redis cache checks, one shared
Redis generation check and 17 native MySQL contracts. Test data used unique
schemas/databases or UUID key prefixes; service processes were not restarted.
Migration scratch helpers likewise report missing configuration as an error.
CI waits for MongoDB's writable primary and includes combined TLS features.

The CLI benchmark creates/removes its own database or temporary SQLite file.
Both MongoDB apps and independent readers honor the URL's database; URI
authentication source is preserved. Ten release smoke trials verified 5,000
timed rows and 100 sampled responses across all database adapters, then
removed their fixtures. Creation/cleanup failure handling is covered by
unit checks. Source, binary and raw evidence are retained in
`benchmarks/evidence/owned-fixtures-20261003/`. These one-pair short runs
cannot meet the performance gate. Required-live crash/recovery scenarios
and final combined/downstream release acceptance remain open.

## Migration recovery inspection and bounded locks: 2026-10-03

`Migrator::inspect_recovery` and `inspectmigrations` read history/progress
without creating tables or taking an advisory lock. The JSON report includes
operation/statement indices, direction, checksum, known-file match and
history status. Missing tables return empty lists; malformed negative or
noninteger progress and invalid directions fail without repairing data.
Execution also rejects progress beyond the operation/statement list.

PostgreSQL/MySQL advisory acquisition now polls nonblocking lock attempts
under a 30-second default deadline. `with_lock_timeout` and CLI
`--lock-timeout-ms` configure acquisition; the deadline excludes pool
acquisition, SQLite BEGIN and work after the lock is acquired. MySQL release
now verifies ownership. Lock timeout is an explicit `MigrationError`.

Five isolated live contracts passed: PostgreSQL atomic rollback after an
owned child-process kill, PostgreSQL non-atomic and MySQL recorded-progress
resume, and held-lock timeout/session release/retry on both engines. Two
SQLite checks passed for fresh read-only inspection and damaged bookkeeping.
No shared service was restarted. Executor responsibilities were split into
locks, progress, operations, SQLite preservation, recovery and test modules;
the parent executor is below 1,000 lines.

Non-transactional explicit SQL and Rust callbacks now record a pending-step
intent before execution. Unconfirmed scripts and ordinary callbacks cannot
be automatically replayed. `register_replay_safe` is an explicit callback
idempotence declaration, including external effects; the framework cannot
verify that declaration. Real child-process checks committed a row before
termination and verified rejection of ordinary callback replay, followed by
safe idempotent recovery. A SQLite simulation also verifies that a script
which commits an INSERT before failing cannot duplicate that row on retry.

Execution requires a pool handle so migration DDL cannot consume a caller's
transaction or connection scope. Inspection still accepts scoped handles.
Twenty-six migration unit tests and three recovery integration tests passed.
G14 remains partial: generated DDL commit/progress and history boundaries,
constrained privileges, disconnects, and documented reconciliation procedures
still require further verification.

## Benchmark worker and adoption gates: 2026-10-03

The runner now exposes concurrency, initial requests, warm-up requests and
Rust scheduler workers. Rust defaults to one worker, matching one Uvicorn
process/event loop; actual startup counts and process IDs are validated and
retained per process and per timed trial. Unknown/different counts cannot
pass adoption/lead gates. Counts do not establish equal total CPU budgets:
SQLite driver workers, blocking pools and shared database CPU are separate.

Plan 3 adoption requires a paired point ratio >=1.10 and a lower confidence
bound above parity. The stronger lead gate requires a lower bound >1.10.
Both now require each measured pair's p99 ratio <=1.05, at least five
identified pairs and trials >=10 seconds. Zero-rounded baseline p99 values
cannot establish a latency ratio. Forty-five Python checks passed.

Plain FastAPI handlers now use async functions for their nonblocking work.
Earlier plain results used the thread-pool path and need refreshed evidence.
An initial SQLite one-lease run overlapped a compilation; it was interrupted
and excluded. The isolated replacement completed five alternating pairs.

SQLite WAL/FULL, pool size one, concurrency 20 and one observed scheduler
worker per application verified 2,050,960 timed committed inserts plus 200
sampled responses. The paired throughput ratio was 1.678x, with a 95%
bootstrap interval of 1.630–1.745x. All trials exceeded ten seconds. Reported
p99 values were 2 ms versus 3 ms; conservative rounding upper bounds were
1.0 in every pair, within the 1.05 budget. Both local adoption and strong
lead gates passed. Evidence lives in
`benchmarks/evidence/sqlite-single-lease-20261003/`. This is one host and one
insert profile, not a lead across databases or production workloads.

The FastAPI SQLite comparator uses a retained native connection with
statement autocommit, foreign keys enabled and matched synchronous policy.
It still blocks its single event loop; mixed workloads and a pooled worker
baseline remain required. Earlier implicit-transaction baseline runs are
not comparable. New trials retain AB percentile CSV at 0.001 ms resolution;
comparison gates account for each source's rounding uncertainty.

The latest full workspace run before these migration/runtime edits passed
910 checks with 84 explicit live-service ignores. Those are an earlier
checkpoint, not verification of the subsequent changes.


## Migration recovery and settings checkpoint: 2026-10-03

Eighteen selected live migration checks passed on private PostgreSQL/MySQL
databases: nine existing migration contracts, seven callback-crash/lock/
caller-transaction contracts and two explicit SQL-script crash contracts.
The latter terminate an owned process after a committed INSERT but before
script completion; recovery reports a SQL intent, rejects retry and leaves
one row. Neither test restarts a service or kills a shared server session.
Strict workspace linting passed; subsequent new migration tests also passed
strict crate linting. Generated-DDL/history boundaries remain open.

Benchmark SQL applications now sample effective settings before readiness:
SQLite version, journal/sync/foreign-key/busy-timeout; PostgreSQL version,
synchronous_commit/fsync/full_page_writes/timezone; MySQL version,
autocommit/timezone/sql_mode/redo-flush/binlog-sync. Startup identity and a
fixed setting-key whitelist guard the retained `database-settings.jsonl`.
These are samples, not proof of every physical pool connection. Forty-six
Python tests and benchmark lint passed; live setting readback is next.


The first precision/settings smoke run was rejected during warm-up: AB's
CSV rounding index can read past its samples for small counts. Live output
contained nonmonotonic garbage percentiles; validation rejected it and no
timed result was published. The runner now requires at least 100 requests
and explicit warm-up requests, and default warm-up clamps to that floor.
Raw invalid evidence remains in `benchmarks/logs/trials-20261003-154143/`.

That run also exposed sampled SQLite versions 3.46.0 (bundled Rust) and
3.53.4 (Python's Homebrew library), despite matching WAL/FULL, foreign keys
and busy timeouts. Earlier local SQLite results retain their measured ratios
but cannot establish a controlled same-engine comparison. The next SQLite
run links both adapters to the installed same engine using the dependency's
explicit pkg-config build override; production defaults are unchanged.


## Matched-engine SQLite insert result: 2026-10-03

The controlled replacement used SQLite 3.53.4 in both applications and
matching sampled WAL/FULL, foreign keys and busy timeout. Five alternating
pairs, pool size one, concurrency 20, warm-up 500 and one observed scheduler
worker verified 1,705,440 timed committed inserts plus 200 response samples.
Every timed trial lasted at least 13.053 seconds. The paired geometric ratio
was 1.574x, with a 95% whole-pair bootstrap interval of 1.501–1.664x. Every
p99 ratio upper bound was below 0.817, meeting the 1.05 budget. Both local
gates passed; source/binary provenance matched and CSV/settings are retained
in `benchmarks/evidence/sqlite-matched-engine-20261003/`.

This supports the SQLx single-lease insert strategy in this cell before a
native SQLite rewrite. It does not settle reader/mixed loads, a pooled-worker
FastAPI baseline, total resource budgets or cross-host/storage replication.
Native MySQL's refreshed matched-worker/settings comparison is underway.


## Refreshed native MySQL result: 2026-10-03

Five alternating one-worker pairs with matching MySQL 8.4.11/session/sync
settings verified 565,540 timed committed rows and 500 response samples.
All timed trials exceeded 13 seconds. The paired throughput ratio was 1.012x,
95% interval 0.965–1.063x; two p99 pairs exceeded the 1.05 budget. Adoption
and strong-lead gates failed. The adapter remains opt-in/experimental.
Earlier unmatched-worker results cannot establish the current goal.
Retained evidence: `benchmarks/evidence/mysql-native-matched-workers-20261003/`.
Next: SQLx under identical controls, then targeted cost decomposition.


Recovery now covers the callback progress-to-intent-clear boundary in both
directions. An explicitly replay-safe registration and validated completed
progress allow clearing a stale intent without rerunning the callback.
Ordinary callbacks/scripts remain blocked. Twenty-eight migration unit
checks and three recovery integration checks pass. The selected live
contracts are being rerun after this refinement, followed by workspace
checks before subsequent timed comparisons.


## PostgreSQL controlled insert result: 2026-10-03

SQLx passed the local insert gates: paired ratio 1.231x FastAPI, 95% interval
1.179–1.286x, with all tail ratios within budget. Five alternating pairs
verified 1,152,100 timed commits and 500 response samples. Observed workers
and sampled PostgreSQL 17.11/session/durability settings matched. Evidence:
`benchmarks/evidence/postgres-matched-workers-20261003/`. This result narrows
the immediate rewrite priority to the unresolved MySQL cost; broader
PostgreSQL workload and pool/component measurements remain necessary.
