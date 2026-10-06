# Framework reliability, CLI and AI tooling roadmap

Reviewed: 2026-10-05. Source baseline: `0f30c32` plus the local gap plan.
Status: planning; C06 and C08 implemented (see checked tasks), others open.

The [developer experience expansion](DEVELOPER_EXPERIENCE.md) adds the
complete Siderite-only daily workflow, 20 additional tasks and AI-focused
acceptance criteria. Together these documents contain 66 proposed tasks.

This plan consolidates the framework gap review and the CLI/AI development
proposal. It separates existing capabilities from proposed work and records
source-backed corrections before the implementation backlog. Historical gap
IDs G14, G18 and G19 identify recovery, modularity and cache retention work;
the actionable remaining scope is included below.

## 1. Critical review and required corrections

### Reuse capabilities that already exist

- OpenAPI 3.1 generation and HTTP documentation already exist through
  `App::openapi()`. Add an export command, not another schema generator.
  Evidence: `crates/siderite-core/src/app.rs`, method `openapi`.
- Offset pagination already exists as ORM `Page<M>` and
  `QuerySet::paginate(page, per_page)`. The useful gap is an HTTP/schema
  integration contract and optional cursor pagination, not a second ORM API.
  Evidence: `crates/siderite-orm/src/queryset/fetch.rs`.
- `scaffold.rs` already implements `new`; extend or split that implementation.
  Do not introduce a competing scaffold engine.
- Streaming responses, static serving and a `TaskQueue` trait already exist.
  SSE, SPA fallback and durable adapters should extend these boundaries.
  Evidence: `crates/siderite-core/src/{responses,static_files,background}.rs`.

### Preserve the CLI/application boundary

`dispatch.rs` delegates app commands through `project.rs` to the application's
compiled `AppCli`. Routes and registered models belong to that application;
a globally installed CLI cannot obtain them by importing its own framework
crates. Update command discovery, parsing, delegation and app execution
together. Export, snapshots and MCP must share the same typed reports.

The product goal is daily application development entirely through the
Siderite CLI, with no direct Cargo commands required. Rust and Cargo remain
build prerequisites managed through clear setup guidance. A prebuilt CLI
and explicit setup flow are included in the developer experience expansion;
no compiler bundling is planned.
Watch mode rebuilds and restarts a process. It is not in-process hot reload.

An offline export means no framework-managed database connection or listener.
The user-provided synchronous app factory can still perform arbitrary work.
Document a side-effect-free factory contract; do not promise sandboxing.

### Correct feature and dependency assumptions

`crates/siderite-cli/Cargo.toml` exposes only `postgres` and `mysql` backend
features. Redis and MongoDB are application-managed stores in
`AppCli::build_app`; inventing CLI features for them would fail compilation.
Preset changes must preserve git/path/version and workspace dependencies.

Do not choose `notify` or a color crate version from the draft. Select and
verify dependencies at implementation time against the declared Rust 1.99
MSRV, licenses and supported platforms. Add direct dependencies only where
used; JSON export alone does not require rebuilding OpenAPI generation.

### Reliability must precede breadth

G19 is P1: generation initialization is non-expiring and invalidation uses
`ttl = None`. Fixing just `route/generation.rs` is insufficient because
`Cache::set_if_absent` explicitly creates non-expiring entries. The trait,
Arc forwarding, MemoryCache and Redis need compatible atomic TTL support.
Finite TTL reduces retention but does not bound unique-target traffic.

G14 already has recovery inspection, intent tracking and live crash tests.
Finish the missing boundaries; do not rebuild recovery or promise exactly-once
DDL on engines with implicit commits. G18's original inventory is stale:
`executor.rs` is now 911 lines; Mongo compilation is still 1,528 lines.
Refresh the inventory before assigning refactors.

Evidence: `crates/siderite-cache/src/{cache,memory,redis}.rs`,
`crates/siderite-cache/src/route/generation.rs`, and G14/G18/G19 in the gap
plan.
The live-test outcomes in that plan are historical claims, not rerun here.

### Narrow the AI and developer experience promises

- Replace "zero hallucinations" with versioned, source-backed context.
- Preserve existing AGENTS.md, CLAUDE.md and editor rules. Default to one
  generated context file, with explicit integration and reviewed updates.
- Export registered metadata, not an unbounded whole-project source dump.
  Exclude credentials, environment values, database rows and user files.
- Existing `route_table` is OpenAPI-derived and omits hidden routes. Call it
  documented routes; do not advertise a complete runtime route inventory.
- MCP v1 is read-only. Defer `scaffold_code` and arbitrary command execution.
  Tool annotations alone are not an authorization boundary.
- A successful `check` is framework validation, not Rust linting. Keep `lint`
  and framework checks distinct in both text and structured output.
- Native database clients are optional except for `dbshell`. Doctor should
  not reject healthy applications merely because `psql` is absent.
- Connectivity and migration status are explicit live probes. Default
  diagnostics and context generation should work without services.
- Defer YAML, built-in SDK generation, Studio and durable adapters until
  their individual contracts are reviewed. Sessions were mentioned but had
  no tasks; include a session/CSRF design decision rather than imply delivery.

## 2. Delivery rules and architecture

Each task is one focused, reviewable change with its own acceptance evidence.
A task that grows beyond that should be split before implementation. Estimates
and owners are intentionally unassigned; dependencies define the order.

Use these shared boundaries:

1. Project selection and process execution in the standalone CLI.
2. Pure report builders for diagnostics, routes, registered models and specs.
3. Text/JSON renderers over those reports; no parsing human terminal output.
4. Application bridge for compiled metadata; explicit live-status operations.
5. Optional MCP transport over the same bounded, validated report interface.

Preserve current commands, text defaults, configuration precedence and exit
codes: 0 success, 1 failure, 2 usage. Define additive versioned JSON envelopes
with `schema_version`, `command`, `ok`, `data` and `diagnostics`. OpenAPI
export
is the raw OpenAPI document, not this envelope. JSON mode reserves stdout;
compiler logs and diagnostics go to stderr. Account for user app-factory
prints by using a separate report channel or rejecting contaminated output.

Keep files below 1,000 lines and lines at most 79 characters. Use modular
helpers, no blanket lint suppression, and Google-style Args/Returns prose
where applicable to new API documentation. Preserve Rustdoc link conventions,
public API documentation, no unsafe code and no library unwrap/expect calls.

## 3. Small task backlog

### A. Reliability and maintainability

- [ ] **R01 — Specify generation expiry and admission invariants.**
  Depends: none. Scope: cache contract and focused regression fixtures.
  Done: expiry/eviction always yields a fresh random token; delayed fills
  cannot revive old data; a concrete cross-process metadata resource bound
  and cache-bypass behavior are specified for unique and uncacheable targets.
  Include retired namespaces and compatibility for external Cache adapters.

- [ ] **R02 — Add atomic expiring insertion to Cache.**
  Depends: R01. Scope: trait, Arc forwarding and MemoryCache.
  Done: unsupported adapters fail explicitly; existing implementations remain
  source-compatible where feasible; expiry and insertion are atomic; tests
  cover concurrent creators, zero/subunit TTL and expiry rounding.

- [ ] **R03 — Implement Redis atomic expiring insertion.**
  Depends: R02. Scope: Redis adapter only.
  Done: initialization uses one atomic operation; live tests prove TTL,
  concurrent creation and cancellation behavior. No GET-then-SET emulation.

- [ ] **R04 — Apply generation lifetime policy.**
  Depends: R03. Scope: generation initialization/invalidation and policy.
  Done: finite lifetimes for both paths; expiry during fills and concurrent
  writes causes safe misses; existing eviction/ABA regression tests pass.

- [ ] **R05 — Enforce and measure metadata bounds.**
  Depends: R04. Scope: R01 admission design and load regression.
  Done: sustained distinct targets, uncacheable responses and namespace churn
  stay within the documented bound; record latency and memory evidence.
  Do not change a shared Redis server's eviction configuration automatically.
  G19 stays open until R01-R05 pass with MemoryCache and live Redis.

- [ ] **R06 — Test DDL-to-progress crash recovery.**
  Depends: none. Scope: existing migration crash harness.
  Done: owned PostgreSQL/MySQL processes die at deterministic boundaries;
  inspection and retry show documented outcomes without unsafe silent replay.
  Preserve existing intent and checksum safeguards.

- [ ] **R07 — Test history-write crash boundaries.**
  Depends: R06. Scope: before/after history recording.
  Done: recovery distinguishes recorded success, rollback and uncertainty;
  operator procedures match observed schema/history, including atomic PG.

- [ ] **R08 — Test migration disconnect and limited privileges.**
  Depends: R06. Scope: lock acquisition/release and permission failures.
  Done: bounded waits, cancellation, diagnostics and release behavior are
  verified on real services; uncertain lock ownership is reported explicitly.
  Update G14 only for boundaries actually exercised.

- [ ] **R09 — Refresh and enforce the size baseline.**
  Depends: none. Scope: file/line inventory and CI guard.
  Done: changed/new code cannot worsen violations; baseline records exact
  existing exceptions and deletion targets, without broad lint suppression.

- [ ] **R10 — Split MongoDB compilation modules.**
  Depends: R09. Scope: private compilation domains, unchanged public API.
  Done: each file satisfies limits; compiler and live Mongo regressions pass.

- [ ] **R11 — Split migration roundtrip tests.**
  Depends: R09. Scope: test modules and shared fixtures.
  Done: discovery preserves every test and relevant backend cases still run.

- [ ] **R12 — Split validation tests.**
  Depends: R09. Scope: validation domains and reusable fixtures.
  Done: runtime and compile-fail coverage remains intact within size limits.

- [ ] **R13 — Split MongoDB integration tests.**
  Depends: R09. Scope: integration-test domains.
  Done: no duplicate fixtures or skipped live cases; all files meet limits.
  Recheck the complete inventory before declaring G18 closed.

### B. Shared CLI contracts and useful first commands

- [ ] **C01 — Make project selection explicit.**
  Depends: none. Scope: project discovery using Cargo metadata.
  Done: nested packages, virtual workspaces, multiple binaries, paths with
  spaces and missing tools have deterministic selection/errors. Specify
  package, manifest and binary flags without breaking app-argument forwarding.

- [ ] **C02 — Add typed reports and output contracts.**
  Depends: none. Scope: shared diagnostics/route/model DTOs and JSON schema.
  Done: stable ordering, envelope version, redaction and exit-code snapshots;
  no new database connections; existing text output remains compatible.

- [ ] **C03 — Expose check/routes JSON end to end.**
  Depends: C01, C02. Scope: parser, APP_COMMANDS, dispatch and AppCli.
  Done: flags before/after commands work or fail clearly; stdout parses as
  one JSON value; errors retain exit semantics; hidden routes remain omitted.

- [ ] **C04 — Export OpenAPI JSON from the app binary.**
  Depends: C01, C02. Scope: `openapi export [--output PATH]`.
  Done: call the existing factory/openapi path without build_app connections,
  server startup or migrations; test mounted routes, conflicts and disabled
  HTTP docs. Validate against the existing OpenAPI schema fixtures. File
  writes are atomic, and existing output is not silently overwritten.

- [ ] **C05 — Expose migration status as structured data.**
  Depends: C02, C03. Scope: migration report API and both CLI entry points.
  Done: selected alias and applied/pending status agree with existing text;
  live access is explicit, bounded and redacted; failures are not empty
  success.
  Do not derive status by scraping printed migration lines.

- [x] **C06 — Add offline doctor diagnostics.**
  Depends: C01, C02. Scope: toolchain, configuration and feature checks.
  Done: compare actual toolchain with manifest MSRV; malformed config and
  missing tools produce actionable text/JSON. DB clients are advisory unless
  requested by dbshell. Port checks are advisory and acknowledge races.
  Summary: `siderite doctor [--json]` (2026-10-05). C01/C02 remain open but
  their project-selection flags and JSON envelope already shipped with X01.

- [ ] **C07 — Add opt-in doctor live probes.**
  Depends: C05, C06. Scope: timeout-limited alias checks and migration status.
  Done: no schema mutation, bounded concurrency, alias-specific results and
  redacted errors; absence of services fails only the explicitly requested
  checks. Document connection-time driver side effects where applicable.

- [x] **C08 — Add fmt/lint/clean passthroughs.**
  Depends: C01. Scope: existing Cargo command runner.
  Done: preserve argument boundaries and child exit status; lint delegates
  to Clippy and is never confused with framework `check`; missing components
  have useful errors. Clean runs only when explicitly invoked.
  Summary: `siderite fmt|lint|clean` reuse the build/test passthrough;
  `lint` maps to `cargo clippy`. Missing rustfmt/Clippy is detected with
  `cargo <sub> --version` and reported with a rustup hint (2026-10-06).

### C. Watch mode and safe project edits

- [ ] **D01 — Implement process supervision.**
  Depends: C01. Scope: build/run state machine and child ownership.
  Done: build failure leaves the last healthy process running; successful
  build triggers graceful shutdown with deadline then forced cleanup.
  Ctrl-C and repeated restart leave no children or held ports on supported OSs.

- [ ] **D02 — Add debounced filesystem watching.**
  Depends: D01. Scope: `dev` and platform-tested watcher dependency.
  Done: watch selected workspace/path dependencies, manifests, lockfile,
  source, config and migrations; ignore target/output/VCS directories.
  Coalesce changes during builds and recover from watcher overflow. Migration
  edits never apply migrations automatically. Test build-restart loops.

- [ ] **D03 — Add dependency add/remove passthrough.**
  Depends: C01. Scope: Cargo-owned manifest edits with explicit package.
  Done: preserve workspace inheritance and dependency source; propagate
  failures; do not claim rollback after partial edits without verifying it.
  Separate package names from presets, for example `add --preset postgres`.

- [ ] **D04 — Add PostgreSQL/MySQL presets.**
  Depends: D03. Scope: only mappings supported by current CLI features.
  Done: preview exact feature/source changes; inherited/path/git dependencies
  remain valid; fixture apps compile. Removal does not discard shared features
  or credentials. Redis/Mongo presets require separate integration designs.

- [ ] **D05 — Extract reusable generation/write helpers.**
  Depends: C01. Scope: existing `scaffold.rs`, templates and write planning.
  Done: dry-run displays exact changes; reject traversal, invalid identifiers,
  symlink escapes and collisions; default writes never overwrite user files.
  Existing `new` fixtures still compile using local framework paths.

- [ ] **D06 — Generate one route module.**
  Depends: D05. Scope: handler and explicit registration snippet.
  Done: generated code compiles against actual public APIs, has docs and a
  unique operation ID; no fragile string-based rewriting of arbitrary routers.

- [ ] **D07 — Generate a minimal model.**
  Depends: D05. Scope: primary key and a documented initial scalar type set.
  Done: derives/attributes compile; model registration is explicit; generated
  metadata works with makemigrations. Reject unsupported types; add UUID/date
  mappings only with verified ORM, Schema and dependency support.

- [ ] **D08 — Generate create/get resource handlers.**
  Depends: D06, D07. Scope: request DTOs and two routes.
  Done: validation, not-found/error mapping and parameter binding pass real
  SQLite fixture tests; no automatic migration execution or auth promise.

- [ ] **D09 — Generate bounded list handlers.**
  Depends: D08, F01. Scope: pagination and stable ordering.
  Done: enforced size limit, correct schema and fixed query budget; no per-row
  ORM queries. Document count/items consistency during concurrent writes.

- [ ] **D10 — Generate update/delete handlers.**
  Depends: D08. Scope: remaining CRUD operations.
  Done: specify PUT/PATCH and absent-vs-null behavior, immutable PK policy,
  not-found responses and status codes. Test malicious field names and partial
  updates. Generated routes are unauthenticated examples until integrated.

### D. AI context and MCP

- [ ] **A01 — Build a bounded application metadata snapshot.**
  Depends: C02, C04. Scope: registered models, documented routes and spec.
  Done: include format/framework version, selected features and completeness
  markers; stable output has size limits and explicit truncation/errors.
  Include alias names/backend kinds only, never URLs, secrets or row data.

- [ ] **A02 — Add ai dump-context.**
  Depends: A01, C01. Scope: JSON/Markdown adapters and app bridge.
  Done: offline by default; unknown/stale data is labeled; optional live
  migration status uses C05 explicitly. Tests cover secret-bearing errors,
  oversized metadata and stdout contamination from application factories.

- [ ] **A03 — Add ai init with one owned context file.**
  Depends: A02, D05. Scope: `.siderite/context.md` and versioned examples.
  Done: examples compile; reruns detect stale versions and show a diff;
  preserve user edits. Do not copy entire source trees or fetch external text.

- [ ] **A04 — Add optional editor instruction links.**
  Depends: A03. Scope: explicit AGENTS/CLAUDE/editor integration options.
  Done: preserve existing content byte-for-byte outside a managed block;
  no default replacement or hierarchy-changing instructions; repeated runs
  are idempotent and conflicting blocks stop with a useful diagnostic.

- [ ] **A05 — Specify and test the MCP transport boundary.**
  Depends: A01. Scope: optional feature/dependency and supported protocol.
  Done: verify official protocol/SDK compatibility at implementation time;
  fixtures cover initialization, version negotiation, capabilities, malformed
  messages, cancellation, size/time limits and shutdown. Stdout is protocol
  only; redact logs. No network listener or arbitrary shell/filesystem tool.

- [ ] **A06 — Expose read-only metadata tools.**
  Depends: A02, A05. Scope: summary, documented routes, models and OpenAPI.
  Done: reuse typed reports via a bounded app bridge; identify stale snapshots;
  initialization alone does not execute a project build. Project execution
  requires explicit trust/selection; capture child logs outside protocol
  output.
  One supported MCP client smoke test verifies enumeration and tool results.

- [ ] **A07 — Expose checks and opt-in migration status.**
  Depends: A06, C03, C05. Scope: framework checks and live read diagnostics.
  Done: checks are not presented as lint; live tools require explicit server
  configuration and selected aliases. Cancellation terminates owned work;
  migration execution and scaffolding tools remain unavailable.

### E. Focused framework additions

- [ ] **F01 — Define and implement HTTP page representation.**
  Depends: none. Scope: reuse ORM Page through an adapter or compatible traits.
  Done: Serialize/Schema work for generic item types without schema-name
  collisions; validate page/size and enforce a configurable maximum. Preserve
  existing ORM API and count-plus-fetch behavior; document concurrency limits.

- [ ] **F02 — Specify cursor pagination semantics.**
  Depends: F01. Scope: design and executable contract fixtures only.
  Done: define stable unique ordering, tie-breaker, nulls, filter binding,
  cursor validation/versioning, backend capabilities and query budgets.
  Implementation is a separate backlog after this design passes review.

- [ ] **F03 — Add opt-in liveness support.**
  Depends: none. Scope: health response and explicit route registration.
  Done: no database dependency, no automatic endpoint collision, minimal
  public information, defined status and tests during startup/shutdown.

- [ ] **F04 — Add bounded readiness orchestration.**
  Depends: F03. Scope: async probe registry and custom checks.
  Done: timeout, concurrency cap and caching prevent per-request connection
  storms; probes reuse existing pools; failures return 503 with redacted
  public output. SQL/Redis checks are optional adapters, not mandatory drivers.

- [ ] **F05 — Add SSE event encoding and response type.**
  Depends: none. Scope: existing streaming response/description hooks.
  Done: correct multiline data, event/id/retry encoding and input validation;
  text/event-stream is documented; stream errors and cache policy are explicit.

- [ ] **F06 — Add SSE keepalive and lifecycle integration.**
  Depends: F05. Scope: idle heartbeat, backpressure and disconnect behavior.
  Done: client disconnect drops work, slow readers do not grow unbounded
  queues, shutdown ends streams, and compression/timeout middleware behavior
  is tested through the HTTP server. Document proxy buffering requirements.

- [ ] **F07 — Add scoped SPA fallback.**
  Depends: none. Scope: existing static_files extension.
  Done: opt-in GET/HEAD HTML navigation fallback; API errors, missing assets
  and other methods retain intended behavior; no path traversal/symlink escape;
  explicit routes win. Test root/prefix mounts and missing index files.

- [ ] **F08 — Decide session and CSRF support.**
  Depends: none. Scope: adapter-versus-owned-implementation design.
  Done: document cookie flags, rotation/revocation, expiry, backend failures,
  CSRF strategy, trusted proxy interaction and application ownership.
  No session implementation is promised by this task.

## 4. Recommended order and release gates

1. Start R01-R05 (P1). R06-R13 and C01-C04 can progress independently.
   Reliability releases must not wait for AI or scaffolding features.
2. Deliver C03/C04 first as useful CLI improvements. Add C05-C08 next.
3. Deliver D01/D02 for watch mode; D03-D07 are separate small increments.
4. Deliver A01-A04 only after structured reports are stable. MCP follows as
   an optional feature after its transport and trust contracts pass tests.
5. Deliver F01 and F03-F07 independently with focused compatibility review.
   Complete CRUD only after generated modules and pagination compile cleanly.
6. Deliver the developer experience expansion alongside these stages,
   following its task dependencies. Its X20 journey is the acceptance gate
   for claiming developers no longer need to invoke Cargo directly.
7. F02/F08 and the deferred items below are design scope, not release promises.

No new API name in this plan is an implemented contract. Each task needs a
short compatibility note, focused tests, and user-facing documentation before
completion. Public changes update maintained `website/` documentation and
CHANGELOG.md; keep in-repo technical notes consistent where relevant.

## 5. Deferred scope with entry criteria

- **YAML export:** add only after JSON export parity and dependency review.
- **Client SDKs:** first evaluate existing generators against exported
  OpenAPI fixtures. A custom TypeScript generator needs its own tasks for
  refs/recursion, nullable unions, parameters, errors, auth, multipart and
  streaming. Never silently reduce unsupported schemas to misleading types.
- **MCP writes:** separate authorization, path containment, preview and audit
  design; do not sneak scaffolding into read-only v1.
- **Studio:** separate authenticated/local access and destructive-action
  design.
- **Storage:** separate streaming, limits, retries, URLs and provider
  contracts.
- **Durable queues:** extend TaskQueue only after delivery, idempotency,
  cancellation and outbox semantics are defined. In-process work is not
  durable.
- **Multi-tenancy:** routing alone does not establish isolation. Require data,
  cache, jobs and authorization isolation design before a routing DSL.

## 6. Verification and definition of done

For implementation PRs, run focused affected-crate tests first, then the
repository-required checks:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Also satisfy existing CI rustdoc, MSRV, dependency-deny and live-backend jobs.
Test minimal/default feature profiles for new optional features, including
MCP disabled. Do not list proposed test filenames as if they already exist.

Use owned disposable services for live Redis, migration and backend tests.
Required live runs must fail when service configuration is absent. Record
simulated, skipped and real-service results separately. Use deterministic
barriers for races/crashes rather than timing-only sleeps.

Compile generated projects against local sources with cargo check, formatting
and Clippy. Exercise route/model/CRUD fixtures on SQLite, and run preset
compilation for PostgreSQL/MySQL. Test workspace selection and process cleanup
on each platform advertised as supported.

Measure new cache operations, watcher idle usage and snapshot size against a
recorded baseline. The draft's HTTP request-rate numbers were not reproduced
for this review and are not release criteria or evidence of readiness.

Review performed: source inspection, current file-size checks and backlog
validation. No runtime tests, benchmarks or live services were run for this
planning-only change. No existing Graphify graph was present to consult;
findings above are grounded in the current source files.
