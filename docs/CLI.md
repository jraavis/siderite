# Command line

Install the CLI, then use it instead of typing `cargo run -- …`:

```bash
cargo install --path crates/siderite-cli
siderite new myapp
cd myapp
siderite run
```

`new` writes an API crate. In a Cargo package, `run`, `routes`, `check`, `dbshell`, `makemigrations` and the other migration commands invoke `cargo run -- <command>` so `AppCli` in your binary sees the app. `build`, `test`, `fmt`, `lint` and `clean` run `cargo build` / `test` / `fmt` / `clippy` / `clean` in the package, with every argument after the command passed through as separate arguments and the child's exit status forwarded (`siderite build --release` replaces `cargo build --release -p myapp`; `siderite lint -- -D warnings` runs Clippy). `lint` is Rust linting and is never confused with the framework's `check`. `fmt` and `lint` first look for `cargo-fmt` / `cargo-clippy` on `PATH` and, when missing, exit `1` with the `rustup component add` command to run. `--bin` applies to `build`, `test` and `lint` only. `clean` runs only when you invoke it. Without a package, `migrate` / `rollback` / `showmigrations` / `squashmigrations` run against JSON files.

| | `siderite` in a package | Standalone (no package) | `AppCli` in your binary |
|---|---|---|---|
| Needs your `App` and models | compiles them via cargo | no | yes |
| Commands | `new`, `doctor`, `run`, `routes`, `check`, `dbshell`, `openapi export`, migrations, `build`, `test`, `fmt`, `lint`, `clean` | `migrate`, `rollback`, `showmigrations`, `squashmigrations` | `run`, `routes`, `check`, `dbshell`, migrations |

Exit codes: `0` success, `1` failure (or `check` found an error), `2` usage error.

## Standalone binary

```bash
cargo install --path crates/siderite-cli --features postgres,mysql
siderite migrate --database-url postgres://app@localhost/app
siderite showmigrations --migrations-dir db/migrations
```

SQLite is always compiled in. PostgreSQL and MySQL are opt-in **cargo features** of `siderite-cli`: `postgres` and `mysql`. The backend is picked by URL scheme (`sqlite:`, `postgres://`, `mysql://`); a URL for a backend that was not compiled in is an error. Error messages never contain the URL, which may hold a password. `squashmigrations` only reads and writes files, so it works without a database URL.

## `AppCli`

```rust
use siderite_cli::{AppCli, CliSettings};
use siderite::prelude::*;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    AppCli::new(build_app)                       // a factory: check builds the app more than once
        .models(&[User::META, Post::META])
        .settings(
            CliSettings::new()
                .addr("0.0.0.0:8000")
                .database("default", "postgres://app@localhost/app"),
        )
        .migrations_dir("migrations")
        .run()
        .await
}
```

`CliSettings` is a thin adapter holding the listen address and the database URLs by alias. Build it from loaded configuration with `CliSettings::from(&siderite::config::load()?)`, which takes `server.addr` and every `databases.<alias>.url`, or set the fields by hand. Its `Debug` output lists aliases only, because URLs can contain passwords. `AppCli::run_from(args)` takes explicit arguments, which is handy in tests.

`run` connects every SQL alias and registers them as the app's `Databases`. Two hooks shape that registry:

```rust
AppCli::new(build_app)
    // Runs once per connected alias, before registration.
    .configure_db(|_alias, db| db.with_signals(receivers::signals()))
    // Routes models to aliases (see DATABASE_ROUTING.md).
    .database_router(AppRouter)
```

## Commands

| Command | What it does |
|---|---|
| `run [--addr ADDR]` | Connects every configured SQL database, registers each under its alias (`App::database`), and serves the app. Aliases whose URL is Redis or MongoDB are skipped: register those yourself in the factory. |
| `routes [--json]` | Prints `METHOD PATH operation_id` for every route, mounts included. |
| `check [--json]` | Validates configuration, models, migrations, routes and the backend (see below). Exits `1` when an error is found. |
| `dbshell` | Starts the database's native client. Replaces Django-style `shell`. |
| `openapi export [--output PATH] [--force]` | Writes the app's OpenAPI 3.1 document (pretty JSON, the same `App::openapi()` that serves `/openapi.json`) to stdout or `PATH`. Builds the app from the factory only: no database connection, server or migration; works with the HTTP docs disabled. |
| `makemigrations [--name SLUG] [--empty] [--dry-run]` | Diffs compiled models against the migration graph and writes a JSON migration. Never connects to a database. |
| `migrate [TARGET] [--dry-run]` | Applies migrations. |
| `rollback [--steps N \| TARGET] [--dry-run]` | Unapplies migrations. |
| `showmigrations [--json]` | `[X]` applied / `[ ]` pending. With `--json`, the same rows as a report (see below). |
| `squashmigrations FROM TO [--name SLUG]` | Collapses a range. Never connects to a database. |

See [MIGRATIONS.md](MIGRATIONS.md) for the migration commands in detail.

### Global flags

| Flag | Meaning |
|---|---|
| `--database ALIAS` | Alias used by `migrate`, `rollback`, `showmigrations` and `dbshell` (default `default`). |
| `--database-url URL` | Database URL, overriding the settings. |
| `--migrations-dir DIR` | Directory of JSON migrations (default `migrations`). |
| `--addr ADDR` | Listen address for `run`. |
| `--help`, `-h` | Help. |

Flags may appear anywhere and take `--flag value` or `--flag=value`.

### Choosing the application

`run`, `routes`, `check`, `dbshell`, `openapi export`, `doctor`, the migration commands and the Cargo passthroughs act on one application package. The project flags below are accepted **before the command** only; everything after the command (or after `--`) is forwarded unchanged, so `siderite run -p 9` still hands `-p 9` to your binary.

| Flag | Meaning |
|---|---|
| `--manifest-path PATH` | Use the workspace or package of this `Cargo.toml` (or of the directory holding one). |
| `--package NAME`, `-p NAME` | Use this workspace member. |
| `--bin NAME` | Run (or, for `build` / `test`, restrict to) this binary of the package. |

A plain package (the nearest `Cargo.toml` with a `[package]`, no `[[bin]]`, `autobins`, `default-run` or `src/bin/`) is used directly, exactly as before, without running `cargo metadata`. Project flags, virtual workspace roots and packages with several binaries select through `cargo metadata --no-deps` (so `cargo` must be on `PATH`; otherwise the command fails with exit `1` and says so). Without project flags, when no package manifest is found above the current directory and `cargo metadata` fails (a broken or unreadable workspace, no `cargo`), commands such as `siderite migrate --database-url ...` still run in standalone mode. Only normal dependencies on `siderite` make a package an application (dev and build dependencies do not). Project flags before `new` are a usage error, and `-p=NAME` / `--bin=NAME` forms are accepted.

Selection is deterministic:

1. `--package` and `--manifest-path` win. A manifest of a virtual workspace falls through to rule 3.
2. Otherwise the package nearest to the current directory (a nested package beats the package around it).
3. Otherwise, from a virtual workspace root or other directory outside every package, the single member that depends on `siderite` and has a binary. Several candidates is a usage error (exit `2`) that lists them and names `--package`; none keeps the usual "needs an application" error.

In the chosen package, `--bin` wins, then `default-run`, then the only binary. Several binaries with neither is a usage error listing them; `build` and `test` do not need a choice. Paths are passed to cargo as separate arguments, so directories with spaces work.

```bash
siderite -p api run                      # from a virtual workspace root
siderite --manifest-path "my app/Cargo.toml" routes
siderite --bin serve check
```

### Precedence

**Listen address** (`run`): `--addr`, then the `ADDR` environment variable, then `CliSettings::addr`, then `127.0.0.1:8000`. Empty values count as unset.

**Database** (`migrate`, `rollback`, `showmigrations`, `dbshell`): `--database-url`, then the URL configured for the selected alias, then, for the `default` alias only, `DATABASE_URL`. With none of those, the command fails with a "no database" error naming the alias.

## Output contract

Commands build typed reports first (`siderite_cli::report::build`, pure functions that open no database connection) and render them second (`report::text`, `report::json`); renderers never parse each other's output. The text output of `check` and `routes` is unchanged and covered by golden tests.

Structured reports share one additive envelope, described by [`docs/schemas/cli-report-v1.json`](schemas/cli-report-v1.json) (JSON Schema draft 2020-12):

```json
{"schema_version":1,"command":"routes","ok":true,"data":{"routes":[]},"diagnostics":[]}
```

- `schema_version` changes only for breaking changes; new fields are additive, so consumers ignore unknown properties.
- `ok` is `false` when any diagnostic has severity `error`. A diagnostic has `code` (the stable `check` ids below), `severity` (`info`, `warning`, `error`), `message` and optional `location` and `hint`. Codes match `^[a-z][a-z0-9_]*\.[EWI][0-9]{3}$`; `info` is reserved and never makes a report fail. Free text is redacted when the diagnostic is built and again when it is rendered: URL passwords (the userinfo ends at the last `@`, so unencoded `/ ? # @`, spaces and quotes are covered), every URL of a comma-separated list, and secret-looking `key=value` pairs anywhere (query parameters, libpq DSNs, `;Pwd=`, `sig=`) become `***`.
- Reports are sorted deterministically: routes by path then method, models by name then table, fields by name then column, tags alphabetically. `routes` lists documented routes (from OpenAPI; hidden endpoints are omitted), not a complete runtime inventory.
- Exit codes: `0` success, `1` failure (including a `check` that finds an error), `2` usage error.
- JSON mode (`--json`, accepted before or after `check`, `doctor`, `routes` and `showmigrations`) writes exactly one JSON value and a newline to stdout; logs and diagnostics go to stderr. The OpenAPI export is the raw OpenAPI document, not enveloped.
- In JSON mode every failure is an envelope as well, with `ok: false`, `data: null` and one diagnostic:

  | Code | Exit | Meaning |
  |---|---|---|
  | `cli.E001` | `2` | usage error (unknown command or flag, `--json` with a command other than `check` / `routes`, with `--help` or without a command) |
  | `cli.E002` | `1` | the command failed before producing its report (for example the OpenAPI document cannot be generated) |
  | `cli.E003` | `1` (`2` if the app exited `2`) | the application exited without a valid report: a build failure, a crash, or a `siderite-cli` that predates `--json` |

- `siderite` runs the application with `SIDERITE_REPORT_PATH` set to a private, freshly created file and sends the child's stdout to stderr. The application writes its report to that file, so cargo output and anything the app factory prints can never corrupt stdout. An application binary run directly writes the report to its own stdout; keep the factory silent in that case.

## Command contract

Every command is declared once in `siderite_cli::commands::COMMANDS`: its name, usage, summary, where it runs (`tool`: the `siderite` binary; `app`: your binary through `cargo run`; `app_or_standalone`: your binary, or JSON migration files without a package; `cargo`: a Cargo subcommand with arguments forwarded unchanged), what it needs (`nothing`, `package`, `app`), what it may change (`project_files`, `output_file`, `database`, `build_artifacts`, `runs_code`; empty means read-only), whether it touches live services (`never`, `opt_in`, `always`) and its output formats (`text`, `json`, `markdown`, `document`). Dispatch, both help texts and `--json` validation read this table.

`siderite commands` prints it as a table; `siderite commands --json` writes a `commands` envelope (`data.version` 1, `data.siderite`, `data.commands`) for AI clients and scripts, described by the report schema.

## Shell completions

`siderite completions bash|zsh|fish` prints a completion script built from the command contract: commands, subcommands (`openapi export`, `ai init`) and each command's flags as declared in its usage, plus the global flags. Nothing is installed and no shell profile is edited; load it yourself, for example:

```bash
siderite completions bash > ~/.local/share/bash-completion/completions/siderite
siderite completions zsh > "${fpath[1]}/_siderite"     # then restart zsh
siderite completions fish > ~/.config/fish/completions/siderite.fish
```

The scripts are tested by running them in bash, zsh and fish.

## `add` and `remove`

`siderite add CRATE... [cargo add options]` and `siderite remove CRATE... [cargo remove options]` run `cargo add` / `cargo remove --package NAME` for the selected app package (`--manifest-path` and `--package` choose it as for other commands), so Cargo makes every manifest edit: workspace inheritance (`workspace = true`), path and git sources and formatting are kept as Cargo keeps them (Cargo may rewrite a path dependency relative to the manifest). The command is printed before it runs, and Cargo's exit code is returned.

`siderite add --preset postgres` (or `mysql`) enables that backend for the app: it runs `cargo add --package NAME siderite-cli --features postgres`, which only adds the feature to the existing `siderite-cli` dependency and keeps its source. A preset needs `siderite-cli` to be a dependency already (it is in projects from `siderite new`), so it never picks a new source. `--dry-run` shows Cargo's exact change without writing it. Presets and crate names do not mix in one call. Several presets run one Cargo command each; a failure stops the rest and lists what was already applied, since nothing is rolled back. `remove --preset NAME` writes nothing: Cargo cannot remove one feature, and removing the dependency would drop features and settings the preset did not add, so it names the line to edit instead. Redis and MongoDB have no presets: they are set up in the app, not through `siderite-cli` features.

## `new`

`siderite new NAME [--database sqlite|postgres|mysql] [--path DIR] [--dry-run]` writes an API crate in `NAME/`, which must not exist or be empty. `--dry-run` prints every file it would create with its full contents (as a diff from nothing) and writes nothing.

Commands that write source files share one write plan: paths are relative with no `..`, may not pass through a symbolic link, may not collide with each other, and an existing file is never overwritten (identical contents are left alone; different contents are a conflict that stops the whole plan before anything is written). Each file is written atomically. Package names must start with a lowercase letter, use only lowercase letters, digits, `-` and `_`, and not be a Rust keyword.

## `dev`

`siderite dev [--grace-ms N] [-- ARGS]` builds the app and runs its binary with `run ARGS`, supervised, and rebuilds when its inputs change. It restarts processes; it is not in-process hot reload.

- Inputs are the files the binary was compiled from, read from cargo's dep-info file next to the executable (every local source file, path dependencies and `include_str!` files included), plus `Cargo.toml`, `Cargo.lock` and `siderite.toml` of the package and workspace root, and the `migrations/` directory. Until the first successful build, the package's `src/` and `build.rs` stand in. The target directory and `.git`, `.hg`, `.svn` and `target` directories inside the workspace are never inputs, so databases, logs and uploads the app writes do not restart it.
- Parent directories are watched non-recursively (saves by rename are seen). A burst of changes is debounced (150 ms quiet, at most 1 s) into one rebuild; changes during a build coalesce into one more. A watcher overflow or error counts as a change rather than being dropped. If the platform watcher cannot start, `dev` keeps running and `r` + Enter still rebuilds.
- Editing a migration restarts the app but never applies it; run `siderite migrate` yourself.
- Each build is `cargo build --message-format=json-diagnostic-rendered-ansi`: compiler diagnostics print as usual and the binary to run comes from cargo's artifact messages (`--bin`, then `default-run`, then the only binary).
- A failed build leaves the last healthy app running. A successful build stops the running app first and waits until it has exited (so its port is free), then starts the new one.
- Stopping sends SIGTERM to the app's process group, waits `--grace-ms` (default 5000), then sends SIGKILL; anything the app started in its group is killed too, and `dev` waits (up to 2 s) until no live member is left.
- Typing `r` and Enter requests a rebuild as well.
- Ctrl-C cancels a running build (cargo and the compilers it started are killed with their process group) and stops the app.
- The app's stdin is not attached; `siderite dev` reads it for the `r` command.

Watching uses the `notify` crate (inotify, FSEvents, ReadDirectoryChangesW). Measured idle cost on Linux: no CPU time over 10 s, about 14 MB resident, 6 threads. Process groups, graceful stop and group cleanup apply on Linux and macOS (tested on Linux). On Windows the app is stopped by force and runs from a copy of the binary, so the next build can replace the original; this path is untested.

## `setup`

`siderite setup [--json] [--install]` checks the build prerequisites and prints the exact next step for this operating system: rustup, `rustc` (at least siderite's `rust-version`), `cargo`, `rustfmt`, `clippy` and a C linker (`cc`, `gcc` or `clang`; on Windows the MSVC linker is reported for information, since rustc finds it through Visual Studio). It never prompts, so a terminal and a script get the same report; `--json` writes a `setup` envelope with the `doctor` data shape. A missing `rustc`, `cargo` or linker exits `1` (`doctor.E001`, `doctor.E002`, `setup.E001`).

Nothing is installed unless you pass `--install`, which runs only `rustup`: `rustup toolchain install stable` for a missing or too old `rustc` (plus `rustup default stable` when there is no toolchain at all) and `rustup component add rustfmt` / `clippy` for missing components. Each command is printed before it runs. Shell profiles and system packages are never changed; without rustup, `setup` prints the rustup installer command instead of running it.

## `doctor`

`siderite doctor [--json]` runs in the standalone binary, so it works even when the app does not compile. It never connects to a database, builds the project or changes anything.

| Check | Fails (error) when | Warns when |
|---|---|---|
| `project` | | project selection is ambiguous or invalid (`doctor.W005`) |
| `toolchain.rustc` | `rustc` is missing (`doctor.E001`), older than siderite's `rust-version` (`doctor.E003`) or the project's (`doctor.E004`) | its version cannot be read (`doctor.W006`) |
| `toolchain.cargo` | `cargo` is missing (`doctor.E002`) | |
| `toolchain.rustfmt`, `toolchain.clippy` | | the component is not installed (`doctor.W001`, `doctor.W002`) |
| `config` | `siderite.toml` or `SIDERITE_*` variables do not load (`doctor.E005`; values of secret keys are not shown) | |
| `database.ALIAS` | the URL has no or an unsupported scheme (`doctor.E006`) | this `siderite` binary was built without that backend's feature (`doctor.W003`) |
| `client.sqlite3`, `client.psql`, `client.mysql` | | never: missing clients are `info`, only `dbshell` needs them |
| `server.addr` | | the address is in use or cannot be bound (`doctor.W004`). Advisory: the port may change state before `run` |

Tools are probed in the project directory (so `rust-toolchain.toml` applies) with a 20-second bound each, in parallel. URLs are never printed, only alias and scheme. The exit code is `1` when any check fails, else `0`; `--json` writes one envelope whose `data.checks` lists `{name, status, detail}` in the order above, with a diagnostic (code, hint) per warning or error.

### `doctor --live`

`siderite doctor --live [--database ALIAS]... [--timeout-ms N] [--migrations-dir DIR]` adds one `live.ALIAS` check per requested alias (every configured alias when none is named) after the offline checks. Each probe connects and reads the migration status with the same code as `showmigrations --json`, bounded by `--timeout-ms` (default 5000) per alias, at most four at a time. Only the requested aliases can fail: an unreachable server, bad credentials or a timeout is `doctor.E007` for that alias, an alias that is not configured is `doctor.E008`, and nothing else in the report changes. Errors name the alias, never the URL.

Probes do not change schema or data: the history table is only read, and SQLite files are opened read-only (`mode=ro`) and never created (a missing file is an error; a relative path is resolved against the project directory). In-memory SQLite, Redis and MongoDB aliases are reported as not probed. Connecting is not entirely free of effects: PostgreSQL and MySQL pools run their session setup (for example setting the session time zone), the server logs the connection, and failed logins count toward any lockout policy; none of this persists past the connection.

## `verify`

`siderite verify [--json] [--live]` runs the whole local verification of the app and reports every step in one place. The steps, in order: `fmt` (`cargo fmt --check`), `lint` (`cargo clippy --all-targets -- -D warnings`), `build` (`cargo build --all-targets`), `test` (`cargo test --all-targets`) and `check` (`cargo run -- check --json`, the framework checks). Every step runs even after an earlier one failed, except that a failed `build` skips `test` and `check`. A missing `rustfmt` or `clippy` fails its step with the `rustup component add` command.

The default profile is offline. `--live` also runs the ignored tests (`cargo test --all-targets -- --include-ignored`), which is where tests that need running services belong. `verify` never formats files, never applies migrations and never connects to a database itself.

Each step records its status (`passed`, `failed` or `skipped`), wall time, exit code and, for a failure, the last 30 lines of its output with credentials redacted. Text output is one line per step, the rendered compiler errors and output of each failed step, and a summary; `--json` writes one `verify` envelope whose `data` is `{profile, diagnostic_format, test_results, steps}` with a `verify.E001` diagnostic per failed step. The exit code is `1` when any step failed. Progress lines go to stderr.

`lint`, `build` and `test` run with Cargo's stable `--message-format=json`, so each step also lists its compiler diagnostics (rustc and Clippy) as `{level, code, message, spans, rendered}`: the error code (`E0308`, `clippy::needless_return`), every source span (`file`, lines, columns, `primary`, `label`) and the full explanation as rustc prints it. Duplicates (Cargo replays cached messages) are dropped, errors come first, and at most 50 are kept per step (`omitted_diagnostics` counts the rest). The `check` step lists the framework check's own diagnostics (`code`, `severity`, `message`, `location`, `hint`) in `framework_diagnostics`, read from the app's report rather than its terminal output. `diagnostic_format` (currently `1`) versions this shape. Individual test results need unstable libtest options, so `test_results` is `aggregate`: a test step reports whether `cargo test` passed as a whole, and its output tail holds the test binaries' own output; nothing is parsed from terminal text.

## `docs search`

`siderite docs search QUERY... [--json] [--limit N]` searches the framework documentation of this CLI's own version, offline. The index is packaged with the CLI and built from the website's start, guides, reference and tutorial pages (one entry per `##` section) and from the workspace examples, which are compiled in siderite's own build (`compiled: true`). Nothing from your project is indexed or sent anywhere.

Ranking is keyword based (BM25 over the section text, with a bonus for words in the title); sections containing every query word come first, and ties are broken by id, so the same query always gives the same order. `--limit` (1 to 50, default 5) bounds the results. Each result carries its id, title, source file and line, a link to that file at the release tag (`source_url`), the Cargo features the section says it needs, the lines matching the query and, in JSON, the section text (cut at 4,000 characters, with `truncated`).

`data.docs_version` is the version the docs describe. When the nearest `Cargo.lock` above the current directory locks a different siderite version, the report adds a `docs.W001` warning naming both versions and the `cargo install siderite-cli --version ...` command for matching docs; the exit code stays `0`.

## `explain`

`siderite explain CODE [--json]` explains a diagnostic code: its cause, the minimal correction and the command that verifies the fix, plus the docs section (`docs search` id) and the first version that emits it. `siderite explain --list` lists every siderite code. Codes match case-insensitively. The catalog is checked against the source by a test, so every code siderite can report has an entry and no entry outlives its code.

Rust compiler codes (`E0308`) and Clippy lints (`clippy::needless_return`) are not explained by siderite: the answer points to `rustc --explain` and the official error-code or lint page. Any other code is `explain.E001` and exits `1`. `--json` writes one `explain` envelope whose `data` is `{siderite, entries}`; each entry has `code`, `source` (`siderite`, `rustc` or `clippy`), `title`, `since`, `cause`, `fix`, `verify`, `docs` and `url`.

## `showmigrations --json`

Works in the app binary and in the standalone `siderite` binary. `data` holds the selected `alias` (`null` in the standalone binary, which has no aliases), where the URL came from (`source`: `flag`, `settings` or `environment`; the URL itself is never printed), the `migrations` as `{id, applied}` in the order of the text output, and `applied` / `pending` counts. Both outputs come from the same `Migrator::show()` rows; nothing parses printed lines.

The read is live but explicit and read-only: it opens the selected database, reads the history table (it never creates it) and is bounded to 30 seconds for connecting and reading together (`cli.E002` with a timeout message after that). Note that the PostgreSQL and MySQL drivers retry a refused connection until that bound. A missing database, an unreadable history or a changed applied file is a failure envelope (exit `1`, or `2` for a missing URL in the standalone binary), never an empty success. Errors name the alias, not the URL.

## `openapi export`

```bash
siderite openapi export > openapi.json
siderite openapi export --output openapi.json          # refuses to replace
siderite openapi export --output openapi.json --force  # replaces atomically
```

- The output is the raw OpenAPI document, not a report envelope, so `--json` does not apply. Paths, operations and components come out sorted, so repeated exports are byte-identical.
- The factory must be side-effect free: export calls it once and only reads the document. Nothing is sandboxed; a factory that connects or prints still does so (prints go to stderr when run through `siderite`).
- `--output` is relative to the directory `siderite` was run in. The file is written to a temporary sibling, synced, then linked into place, so readers never see a partial document. An existing file is never replaced without `--force`, including one created concurrently; `--force` replaces it with an atomic rename.
- A document that cannot be generated (for example duplicate operation ids) exits `1` with nothing on stdout.

## `ai dump-context`

`siderite ai dump-context [--format markdown|json] [--live] [--max-bytes N]` (`--json` is the same as `--format json`) prints the application snapshot below as context for AI tools: Markdown by default (headings and tables for databases, models with their fields, documented routes, migration status and notes), or the `ai dump-context` envelope whose `data` holds the `snapshot`, its `source` (binary name and build time, so a stale context can be spotted) and `migrations`.

- Offline by default: nothing connects, and `migrations.state` is `unknown`. `--live` reads the selected database's migration status through the `showmigrations --json` code (read-only, bounded to 30 s); a failure is `state: failed` with a redacted `ai.E001` error and exit `1`, never an empty success.
- Secrets never reach the output: database URLs appear as alias and backend kind only, and descriptions, summaries and every OpenAPI string are redacted.
- `--max-bytes` bounds the snapshot; anything cut is marked incomplete in both formats, with a `snapshot.W001` note.
- Through `siderite`, the context is the only stdout output; cargo and anything the app factory prints go to stderr. When the app writes nothing (a build failure), stdout stays empty and the exit code is `1` (`--format json` output must be valid JSON).

## `ai init`

`siderite ai init [--check] [--dry-run] [--force]` writes `.siderite/context.md`, the one context file `siderite` owns in a project, for AI assistants and editors to read. The file has a generated block between `<!-- siderite:context begin ... -->` and `<!-- siderite:context end -->`: the commands to use, framework conventions, this project's databases, models and documented routes (from the snapshot, without timestamps), and examples for this framework version. The examples live in `crates/siderite-cli/src/ai/examples/` and are compiled and exercised in the test suite, so they match the release they ship with. Nothing is fetched from the network and no source files are copied.

- Everything outside the markers belongs to you and is kept byte for byte; a new file starts with a "Project notes" section for that.
- The begin marker records `format`, the `siderite` version and a hash of the block. A rerun reports the file as up to date, stale (another framework version, or the project changed; the diff is shown and the block replaced) or edited by hand (the block differs from its hash: nothing is written and the diff is shown; `--force` replaces the block and still keeps your text outside it). A file without markers is never touched.
- `--check` writes nothing and exits `1` when the file is missing or stale (for CI); `--dry-run` shows the exact change. The file is written through the shared write plan (no symbolic-link escapes, atomic).
- It runs in the app binary, so through `siderite` the file lands in the package directory.

## `ai context`

`siderite ai context --task TEXT [--budget BYTES] [--include PATH]... [--json]` builds context for one task. Everything is selected locally by keyword and nothing is sent anywhere.

- Parts, in priority order: a project summary (app, siderite version, database aliases and backends, counts); the models whose name, table or fields match the task words (name matches count double; the rest are listed by name); the matching routes (the rest are counted); the files named with `--include`; the best four documentation sections and two compiled examples of this siderite version (as in `docs search`).
- The budget unit is bytes of the Markdown output (default 32768, from 4096 to 4194304). Parts are taken in order; the first that does not fit is cut at a line boundary (an open code block is closed) when at least 400 bytes are left, and later parts that do not fit are listed under "Not included" with their size. The whole output, that list included, stays within the budget, and the same inputs always give the same output.
- Each part names its source and version: compiled app metadata, a docs file and line, an example compiled in siderite's build, or the included file. "Not included" also lists what is missing: migration status (never read here), source files when none were included, and anything that matched nothing.
- `--include` takes files or directories, relative to where you run `siderite`. Only paths inside the app package are read; files git ignores are skipped (without git, `target`, `node_modules` and hidden directories are), names that usually hold secrets (`.env*`, `*.pem`, `*.key`, `id_rsa*`, `*.local.toml`, ...) are refused even when tracked, as are binary files and files over 1 MiB, and at most 200 files are read. Contents are redacted like every other output.
- `--json` writes an `ai context` envelope whose `data` holds `task`, `siderite`, `docs_version`, `budget` (`unit`, `limit`, `used`), `parts`, `omitted`, `missing` and the `markdown` itself.

## `ai validate`

`siderite ai validate [--json] [--live]` runs exactly the `verify` pipeline (same steps, order and profiles; no rules of its own) and reports it as findings for an AI tool. Each finding has the step, level, code, message, location (`file:line:column` of the primary compiler span, or the framework diagnostic's location), what `siderite explain` says about the code (its fix, and the docs section or the official rustc or Clippy page) and the command that reproduces it. Compiler errors and the framework check's errors and warnings become findings; a failed step without either (formatting, a test failure) is one finding pointing at its command. The same compiler error reported by `lint` and `build` appears once. Findings are bounded to 50, errors first (`omitted_findings` counts the rest).

`commands` lists the failed steps' commands, then `siderite verify` for the whole run. Nothing is fixed automatically, and every report, a clean one included, states that passing these checks does not prove a change correct: application logic still needs task-specific acceptance tests. The exit code is `verify`'s.

## Application snapshot

`siderite_cli::report::snapshot::snapshot(app, models, settings, limits)` builds a bounded, versioned description of an application for tools and AI context (`ai dump-context` renders it). It reads compiled metadata only and opens no connection. The `snapshot` envelope's `data`:

| Field | Content |
|---|---|
| `format`, `format_version` | `siderite-snapshot`, `1` (bumped only for breaking changes) |
| `framework` | `name` and `version` of the framework compiled into the app |
| `app` | OpenAPI `title`, `version`, `description` (redacted) |
| `features` | `siderite-cli` features compiled into the app binary (`mysql`, `postgres`) |
| `databases` | configured aliases with their backend kind; never URLs |
| `models`, `routes` | the `models` and `routes` reports (documented routes only) |
| `openapi` | the OpenAPI document with every string redacted, or `null` |
| `completeness` | per section: `complete`, `total`, `included`, `reason` |

`Limits` bound the result (defaults: 1 MiB in total, 500 models, 2,000 routes, 512 KiB of OpenAPI). Items are cut from the end of their sorted order, so output is deterministic; each cut is recorded in `completeness` with a `snapshot.W001` warning. The total budget drops the OpenAPI document first, then routes, then models. An OpenAPI document that cannot be generated is a `snapshot.E001` error with empty, incomplete routes and spec; models are still reported.

## `check`

`check` runs without starting a server or opening a database. Each issue prints as `error: [models.E003] message` or `warning: [id] message`, followed by a summary line. Messages never contain database URLs.

| Id | Level | Meaning |
|---|---|---|
| `config.E001` | error | models are registered but no `default` database is configured |
| `config.E002` | error | a database URL has no scheme or does not parse |
| `config.E003` | error | a database URL uses an unsupported scheme |
| `models.E001` | error | two models use the same table |
| `models.E002` | error | a model name is registered more than once |
| `models.E003` | error | a model has no primary key |
| `models.E004` | error | a foreign key points to a model that is not registered |
| `models.E005` | error | a many-to-many relation (or its through model) targets an unregistered model |
| `models.E006` | error | two fields map to the same column |
| `models.E007` | error | a model has more than one primary-key field |
| `migrations.E001` | error | the migration files cannot be loaded |
| `migrations.E002` | error | the migration dependency graph is invalid |
| `migrations.E003` | error | the migrations do not replay cleanly |
| `migrations.W001` | warning | model changes are not recorded in any migration; run `makemigrations` |
| `openapi.E001` | error | the OpenAPI document cannot be generated (duplicate operations or operation ids) |
| `routes.E001` | error | the app cannot be built into a router (duplicate routes, malformed paths) |
| `backend.E001` | error | the `default` database is Redis, which cannot hold models |
| `backend.E002` | error | a model needs joins the `default` backend does not support |
| `backend.E003` | error | the schema cannot be created on the `default` backend |
| `backend.W001` | warning | the backend has no schema migrations; `migrate` will refuse to run |

Migration checks are skipped when no migrations directory is passed to the library function `siderite_cli::check`; `AppCli` always passes its own.

## `dbshell`

`dbshell` starts `sqlite3`, `psql` or `mysql` on the selected database and returns the client's exit code. Passwords never appear on the command line or in output: `psql` receives `PGPASSWORD` and `mysql` receives `MYSQL_PWD` in its environment. A userinfo password and a PostgreSQL `?password=` query parameter both move into `PGPASSWORD` and are stripped from the URL passed to `psql`, so options such as `sslmode` still apply. A URL that carries `sslpassword` is refused: there is no safe way to pass it. For MySQL only host, port, user and database are forwarded. An in-memory SQLite URL, an unknown scheme, or a client that is not on `PATH` is an error. MongoDB and Redis are not supported.
