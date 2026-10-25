# The Cargo-free journey

Status of the developer journey that should need no direct Cargo
command, checked end to end by `crates/siderite-cli/tests/journey.rs`
(ignored by default; the `journey` CI job runs it on Linux).

## The journey

| Step | Command |
|---|---|
| Diagnose prerequisites | `siderite setup`, then `siderite doctor` |
| Scaffold | `siderite new shop --template api` |
| Develop | `siderite dev` (rebuilds and restarts; the test waits for `/livez`) |
| Add a resource | `siderite generate resource book title:String pages:i32? --table books` |
| Register it | the two source edits the generator prints, pasted as printed |
| Create and apply a migration | `siderite makemigrations`, `siderite migrate`, `siderite showmigrations` |
| Generate tests | `siderite generate test resource book ...` |
| Prepare AI context | `siderite ai init`, `siderite ai context --task "..."` |
| Validate | `siderite verify` (fmt, lint, build, test, framework checks) |
| Package | `siderite package --release` (after the build created `Cargo.lock`) |

No step invokes `cargo` directly; `siderite` runs Cargo where it needs it.

## Product gaps

Steps that still need Cargo, rustup or manual work:

- **Installing the CLI** is `cargo install siderite-cli` (with
  `--features mcp` for the MCP server). There are no prebuilt binaries
  yet.
- **Installing Rust** is rustup's job. `siderite setup` prints the exact
  rustup command, and `siderite setup --install` runs rustup for a
  missing or old toolchain or component, but rustup itself has to be
  installed first.
- **Registering generated code** is a manual edit by design: siderite
  prints the lines and never edits existing source. Before this fixture,
  the printed model registration did not compile from `src/main.rs`; it
  now prints `<CRATE::models::NAME::Type as siderite::orm::Model>::META`.

## Baselines

The journey test also measures the developer-experience baselines and
writes them as JSON to the file named by `SIDERITE_JOURNEY_REPORT`; the
`journey` CI job uploads that file as the `journey-baseline` artifact.

| Measure | What is timed |
|---|---|
| First run | `siderite new` until `siderite dev` first answers `/livez` |
| Edit to restart | moving the liveness route while `dev` runs, until the new path answers; measured twice (there and back) |
| Context size | bytes of `ai context --task "add an author to books"` against its default budget |
| Verify | `siderite verify` (fmt, lint, build, test, framework checks) |

Recorded on 2026-10-25 at `6c48173`: Linux x86_64, 4 CPUs, rustc
1.99.1, debug builds sharing the workspace `target` directory.

| Measure | Run 1 | Run 2 |
|---|---|---|
| First run | 7.5 s (scaffold 0.1 s) | 7.8 s (scaffold 0.1 s) |
| Edit to restart | 2.3 s, 2.1 s | 2.3 s, 2.2 s |
| Context size | 18,210 of 32,768 bytes, nothing omitted | same |
| Verify | 43.7 s | 5.6 s |

Both runs reused dependencies an earlier run had compiled, so first run
here is a warm-cache figure; a machine with an empty Cargo cache adds a
full dependency build, which is not measured. Run 1's `verify` built its
lint and test artifacts for the first time; run 2 reused them. Treat
these as baselines to compare against, not targets.

## Operating systems

No operating system is advertised as supported yet. The journey is
exercised on Linux (the only CI platform). macOS shares the Unix process
handling `siderite dev` relies on but is not exercised; on Windows, `dev`
stops the app by force and is untested. Add a platform to the `journey`
CI job before advertising it.
