# Siderite developer experience: one CLI and AI-ready applications

Updated: 2026-10-05. Status: X01-X05, X11 (and parent C03, C06, C08) implemented; other tasks proposed.
Parent: [reviewed roadmap](CLI_AI_ROADMAP.md).

## Product goal

A developer should be able to create, run, change, test and package a Siderite
application using `siderite` commands alone. Cargo remains the build engine
underneath; learning its command syntax should not be required for routine
application development. Rust knowledge still matters for custom code.

Provide a prebuilt Siderite CLI as an installation option. Detect the Rust
build toolchain and explain missing prerequisites through `siderite setup`.
Toolchain installation is explicit; setup never silently changes global Rust
versions. Built applications run without Cargo or the developer CLI installed.

AI tools should discover the exact installed framework APIs, understand the
current project, receive compiler diagnostics, and validate their changes
through the same commands developers use. Start with local, provider-neutral
integration. An embedded chat UI or paid model subscription is not required.

## Intended everyday workflow

The following is the target experience. Existing commands and proposed
extensions are distinguished in the table below.

```bash
siderite setup
siderite new shop --template api --database sqlite
cd shop
siderite dev
siderite generate crud Product name:string price:decimal
siderite makemigrations --name products
siderite migrate
siderite test
siderite verify
siderite ai init
siderite ai context --task "Add a product search endpoint"
siderite openapi export --output openapi.json
siderite package --release
```

Decimal CRUD generation is a later verified type mapping, not part of the
initial scalar generator. Run dev in a separate terminal. Database changes
remain explicit commands; development reload never applies them implicitly.

| Developer need | Target interface | Baseline / delivery |
|---|---|---|
| New application | `new --template api` | `new` exists; templates are new |
| First-time setup | `setup`, `doctor` | Exist (X02, C06) |
| Start application | `run` | Exists |
| Rebuild on changes | `dev` | Proposed D01-D02 |
| Build and test | `build`, `test` | Exist; richer reports proposed |
| Add libraries | `add`, `remove` | Proposed D03-D04 |
| Generate application code | `generate route/model/crud` | Proposed D05-D10 |
| Format and lint | `fmt`, `lint`, `clean` | Exist (C08) |
| Complete local verification | `verify` | Exists (X04) |
| Framework checks | `check --json` | Exists (C03) |
| Database changes | `makemigrations`, `migrate`, `rollback` | Exist |
| Demo data | `seed --dataset demo` | New X09 |
| Local backing services | `services up/down/status` | New X08 |
| Effective config | `config show --redacted` | New X07 |
| Route/spec inspection | `routes --json`, `openapi export` | `routes --json` exists (C03); export C04 |
| API compatibility | `openapi diff BASE CURRENT` | New X16 |
| Framework help | `docs search`, `explain CODE` | New X11-X12 |
| AI setup/context | `ai init`, `ai context --task TEXT` | A03 and X13 |
| AI tools | `mcp` | Proposed A05-A07 |
| Release artifact | `package --release` | New X17 |

Command names are proposals. Preserve existing spellings as compatible entry
points. Do not require developers to relearn working migration commands.

## Recommended additions and small tasks

Dependencies refer to task IDs in the parent roadmap or this document. Each
checkbox is a separate reviewable change. Checked tasks are implemented.

### Make the CLI sufficient for daily work

- [x] **X01 — Define the developer command contract.**
  Depends: C01, C02. Scope: consistent help, arguments and command metadata.
  Done: commands declare project requirements, mutations, live access and
  supported output modes. Provide a versioned machine-readable command list
  for AI clients. Preserve Cargo argument forwarding and exit semantics.

- [x] **X02 — Add setup diagnostics.**
  Depends: C06. Scope: `setup` as a guided prerequisite check.
  Done: identify missing compiler/linker/platform prerequisites and compatible
  versions; show exact next steps. Interactive and non-interactive runs agree;
  no shell profile or global toolchain change without explicit installation.
  Summary: `siderite setup [--json]` reuses C06's toolchain checks against the
  framework MSRV and prints ordered next steps. It reads no input and never
  installs; tests use stub toolchains on a fake PATH. Windows linker: skipped.

- [x] **X03 — Add shell completions.**
  Depends: X01. Scope: Bash, Zsh and Fish completion generation.
  Done: derive completions from command metadata, test nested subcommands,
  and never edit shell profiles by default.
  Summary: `siderite completions <bash|zsh|fish>` prints a script built from
  `command_catalog()` and `global_flags()`; metadata gains optional `values`
  and `subcommands`. Tests execute the Bash (3.2+) script and the Zsh script
  (with stubbed compsys functions) on a nested fixture; Fish output is checked
  as text, plus `fish -n` when installed. No real interactive-shell test yet.

- [x] **X04 — Add one-command verification.**
  Depends: C03, C08. Scope: `verify` orchestration.
  Done: run formatting checks, lint, compilation, tests and framework checks
  through existing runners; return one aggregate report with per-step status,
  durations and failures. Offline is default; required live checks are an
  explicit profile. No automatic formatting edits or migrations.
  Summary: `siderite verify [--json]` runs fmt --check, clippy -D warnings,
  build, test and `check --json` through the passthrough runner; test/check
  are skipped after a build failure and contaminated check stdout fails the
  step. Offline only: no live profile until C05/C07 provide live checks.

- [x] **X05 — Normalize compiler and test reports for agents.**
  Depends: C02, X04. Scope: Cargo structured diagnostics and test reporting.
  Done: preserve error codes, source spans and rendered explanations with
  explicit format versions. Use a supported stable test-report mechanism;
  if individual test results are unavailable, report aggregate status instead
  of inventing a fragile terminal-output parser or requiring nightly Rust.
  Summary: `verify --json` runs lint/build with `--message-format=json` and
  reports deduplicated compiler/Clippy diagnostics (level including ICEs,
  code, spans with byte offsets, suggested replacements and applicability,
  child notes, rendered text; capped at 100 per step)
  under `format_version` 1 and `diagnostics_format`
  `cargo-json-diagnostics/1`. rustc's long `--explain` text is dropped on
  purpose. Tests are `test_results: aggregate` (stable libtest has no JSON).
  Text mode is unchanged. Fixtures are real cargo output. C02 remains open.

- [ ] **X06 — Add a verified API project template.**
  Depends: D05. Scope: `new --template api` and a template version manifest.
  Done: include one tested endpoint, configuration example and developer
  instructions; generated project passes verify. Keep the existing default
  template compatible. Additional templates must each pass the same checks.

- [ ] **X07 — Explain effective configuration.**
  Depends: C02, C06. Scope: `config show --redacted` and `config validate`.
  Done: display resolved non-secret values with their source/precedence;
  redact credentials and sensitive query parameters even on parsing errors.
  JSON and text agree. Do not introduce a second configuration loader.

- [ ] **X08 — Manage optional local service fixtures.**
  Depends: C07, X06. Scope: one PostgreSQL development-service recipe first.
  Done: explicit container-runtime prerequisite; project-scoped resources,
  readiness waits and random/local credentials; no exposed nonlocal ports by
  default. Down preserves data; data deletion requires a separate explicit
  command. Add Redis/MySQL recipes only after independent fixture tests.

- [ ] **X09 — Add explicit seed datasets.**
  Depends: C01, D07. Scope: app-registered `seed --dataset NAME` callbacks.
  Done: selected database alias and dataset are visible; deterministic fixtures
  support documented idempotence and transaction behavior where available.
  No production/default reset, automatic execution or arbitrary SQL ingestion.
  Seeding does not masquerade as a reversible migration.

- [ ] **X10 — Generate a request/response test.**
  Depends: D06, X06. Scope: `generate test` using existing testkit APIs.
  Done: generated test compiles, calls the route, checks status/body and
  demonstrates a failure case. Database fixtures remain opt-in and disposable.
  Avoid generating only tests that repeat the handler implementation.

### Help AI tools produce code that actually compiles

- [x] **X11 — Ship version-matched framework documentation.**
  Depends: X01. Scope: `docs search QUERY` over a packaged local index.
  Done: results carry framework version, feature requirements and source
  locations; runnable examples are compiled during release verification.
  Local search works offline; installed-version mismatch is clearly reported.
  Index only maintained public API documentation, not user secrets/source.
  Summary: `siderite docs search QUERY [--limit N] [--json]` searches a
  checked-in `docs-index.json` (crate-local, so it ships with the published
  CLI) built from the website's start/guides/reference/tutorials pages; a
  unit test fails when it drifts from the guides or the crate version. Hits
  carry framework version, path:line, site URL and the cargo features a
  section names; `--full` returns the section text. Queries drop stop words,
  stem endings and fall back to partial matches (`match_mode`). Project
  `Cargo.lock` mismatch (`siderite`, else `siderite-core`) warns. Gap: doc
  snippets are not compiled; only the workspace `examples/` are.

- [ ] **X12 — Explain framework diagnostics.**
  Depends: X11, C02. Scope: `explain CODE` for stable framework error codes.
  Done: each entry describes cause, minimal valid correction and verification
  command, tied to supported versions. Unknown/compiler codes link or point
  to the appropriate source rather than invent framework explanations.

- [ ] **X13 — Produce task-focused AI context.**
  Depends: A02, X11. Scope: `ai context --task TEXT --budget N`.
  Done: select relevant public examples plus project metadata; use a stated
  budget unit and deterministic truncation; include source/version evidence
  and missing information. Begin with local keyword retrieval. Source-file
  inclusion is explicit, honors ignore rules and stays within project roots.
  Do not send project contents to an external model by default.

- [ ] **X14 — Add a bounded AI validation report.**
  Depends: X05, X13. Scope: `ai validate` over the existing verify pipeline.
  Done: reports compile/lint/test/check failures with source spans and relevant
  documentation; produces reproducible commands without automatically applying
  fixes. No separate validation rules or claims that passing proves
  correctness.
  Changes to application logic still need task-specific acceptance tests.

- [ ] **X15 — Expose docs and diagnostics through MCP.**
  Depends: A06, X11, X12. Scope: read-only search_docs and explain_diagnostic.
  Done: return the same versioned results as CLI commands, bounded by size and
  cancellation limits. Add command-discovery metadata from X01. Running verify
  remains a separate explicit project-execution capability, not a read tool.

### Make changes and releases easier to review

- [ ] **X16 — Compare exported OpenAPI contracts.**
  Depends: C04. Scope: `openapi diff BASE CURRENT` for local JSON files.
  Done: report removed paths/operations, newly required inputs and changed
  response contracts with source pointers. Document supported schema cases;
  unresolved/complex comparisons are unknown, not compatible. No remote-ref
  fetching by default. Test both breaking and additive fixture changes.

- [ ] **X17 — Package a reproducible release artifact.**
  Depends: C01, X04. Scope: `package --release` for the host target first.
  Done: explicit locked-dependency policy; include binary, declared static
  assets and migration files with checksums/version manifest; omit credentials,
  databases and build caches. Smoke-test without Cargo installed. Packaging
  does not deploy or apply migrations; native runtime dependencies are listed.

- [ ] **X18 — Generate a project CI workflow.**
  Depends: X04, X06. Scope: one supported CI provider template.
  Done: CI runs the same verification contract with pinned project toolchain,
  explicit live-test jobs and useful artifacts; preserve existing workflow
  files unless an explicit reviewed update is requested. No embedded secrets.

- [ ] **X19 — Preview framework upgrade impact.**
  Depends: X11, D03. Scope: `upgrade --dry-run` for an explicit target version.
  Done: show dependency changes, compatibility notes and verification commands
  without writing files. Handle git/path/workspace sources explicitly; unknown
  compatibility is reported. Automatic source rewriting is separate scope.

- [ ] **X20 — Verify the Cargo-free user journey.**
  Depends: X02, X04, X06, X10, X14, X17, D02, D10, A03.
  Scope: documented end-to-end acceptance fixture.
  Done: install CLI, diagnose prerequisites, scaffold, run dev, add a resource,
  create/apply a migration, generate tests, prepare AI context, validate and
  package using only siderite commands. Record any step requiring direct Cargo
  use as a product gap. Exercise each advertised supported operating system.

## Delivery priorities

**First useful release:** C01-C04, C06, C08, D01-D02, A01-A03, X01-X05,
X11-X14. This gives one-command development, verification, useful diagnostics
and version-correct AI assistance before investing in larger generators.

**Second release:** D03-D10, F01, X06-X10, X16-X18. Focus on complete resource
workflows and reproducible application delivery. Keep independent commands
releasable; the release grouping does not override task dependencies.

**Following release:** MCP A05-A07 plus X15, upgrade preview X19, and the
full journey X20. Complete X20 only when all prerequisites have shipped.
These product tracks can proceed alongside the parent's reliability work;
G19 remains the urgent cache priority, not a blocker for unrelated CLI work.

## Suggestions to evaluate after this scope

- Client generation through a verified existing OpenAPI generator adapter.
  A built-in TypeScript generator is a separate maintenance commitment.
- Authenticated API starter after the session/CSRF decision F08, with explicit
  authorization and negative tests. Never label basic CRUD production-secure.
- Structured development request logs with redaction and correlation IDs.
  Decide how to integrate existing tracing before adding another logger.
- An optional guided `siderite ai implement` workflow only after context and
  validation are dependable. Design provider/credential ownership, budget,
  workspace diff review and execution permissions first. Keep ordinary AI
  assistance usable with external agents and no built-in provider account.

## Success measures

- A fresh application completes X20 without the developer invoking Cargo.
- All generated templates and examples compile against their stated version.
- Agents can discover commands, retrieve relevant APIs and receive structured
  errors without guessing unsupported framework syntax.
- Context output is bounded and omits secrets in positive and failure tests.
- Existing projects keep their source, instructions and dependency ownership.
- Measure first successful run, edit-to-restart latency, context size and
  verification duration on documented fixtures before setting numeric targets.

This document adds 20 tasks to the parent's 46. Implemented so far: X01
(command contract), X02 (setup diagnostics), X03 (shell completions),
X04 (verify), X05 (structured diagnostics) and X11 (docs search),
plus the parent's C03 (`check`/`routes` JSON), C06 (`doctor`) and C08
(`fmt`/`lint`/`clean`).
