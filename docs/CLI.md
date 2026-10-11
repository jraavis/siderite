# Command line

Install the CLI, then use it instead of typing `cargo run -- …`:

```bash
cargo install --path crates/siderite-cli
siderite new myapp
cd myapp
siderite run
```

`new` writes an API crate. In a Cargo package, `run`, `routes`, `check`, `dbshell`, `makemigrations` and the other migration commands invoke `cargo run -- <command>` so `AppCli` in your binary sees the app. `build` and `test` are `cargo build` / `cargo test` in the package, with every argument after the command passed through (`siderite build --release` replaces `cargo build --release -p myapp`). Without a package, `migrate` / `rollback` / `showmigrations` / `squashmigrations` run against JSON files.

| | `siderite` in a package | Standalone (no package) | `AppCli` in your binary |
|---|---|---|---|
| Needs your `App` and models | compiles them via cargo | no | yes |
| Commands | `new`, `run`, `routes`, `check`, `dbshell`, migrations, `build`, `test` | `migrate`, `rollback`, `showmigrations`, `squashmigrations` | `run`, `routes`, `check`, `dbshell`, migrations |

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
| `makemigrations [--name SLUG] [--empty] [--dry-run]` | Diffs compiled models against the migration graph and writes a JSON migration. Never connects to a database. |
| `migrate [TARGET] [--dry-run]` | Applies migrations. |
| `rollback [--steps N \| TARGET] [--dry-run]` | Unapplies migrations. |
| `showmigrations` | `[X]` applied / `[ ]` pending. |
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

`run`, `routes`, `check`, `dbshell`, the migration commands, `build` and `test` act on one application package. The project flags below are accepted **before the command** only; everything after the command (or after `--`) is forwarded unchanged, so `siderite run -p 9` still hands `-p 9` to your binary.

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
- JSON mode (`--json`, accepted before or after `check` and `routes`) writes exactly one JSON value and a newline to stdout; logs and diagnostics go to stderr. The OpenAPI export is the raw OpenAPI document, not enveloped.
- In JSON mode every failure is an envelope as well, with `ok: false`, `data: null` and one diagnostic:

  | Code | Exit | Meaning |
  |---|---|---|
  | `cli.E001` | `2` | usage error (unknown command or flag, `--json` with a command other than `check` / `routes`, with `--help` or without a command) |
  | `cli.E002` | `1` | the command failed before producing its report (for example the OpenAPI document cannot be generated) |
  | `cli.E003` | `1` (`2` if the app exited `2`) | the application exited without a valid report: a build failure, a crash, or a `siderite-cli` that predates `--json` |

- `siderite` runs the application with `SIDERITE_REPORT_PATH` set to a private, freshly created file and sends the child's stdout to stderr. The application writes its report to that file, so cargo output and anything the app factory prints can never corrupt stdout. An application binary run directly writes the report to its own stdout; keep the factory silent in that case.

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
