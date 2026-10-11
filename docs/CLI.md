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
