# Command line

Install the CLI, then use it instead of typing `cargo run -- …`:

```bash
cargo install --path crates/siderite-cli
siderite new myapp
cd myapp
siderite run
```

`new` writes an API crate. In a Cargo package, `run`, `routes`, `check`, `dbshell`, `makemigrations` and the other migration commands invoke `cargo run -- <command>` so `AppCli` in your binary sees the app. `build`, `test`, `fmt`, `lint` and `clean` run `cargo build` / `test` / `fmt` / `clippy` / `clean` in the package, with every argument after the command passed through (`siderite build --release` replaces `cargo build --release -p myapp`). Without a package, `migrate` / `rollback` / `showmigrations` / `squashmigrations` run against JSON files.

| | `siderite` in a package | Standalone (no package) | `AppCli` in your binary |
|---|---|---|---|
| Needs your `App` and models | compiles them via cargo | no | yes |
| Commands | `new`, `run`, `routes`, `check`, `dbshell`, migrations, `build`, `test`, `fmt`, `lint`, `clean`, `verify` | `migrate`, `rollback`, `showmigrations`, `squashmigrations` | `run`, `routes`, `check`, `dbshell`, migrations |

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
| `routes [--json]` | Prints `METHOD PATH operation_id` for every documented route, mounts included. |
| `check [--json]` | Validates configuration, models, migrations, routes and the backend (see below). Exits `1` when an error is found. |
| `dbshell` | Starts the database's native client. Replaces Django-style `shell`. |
| `makemigrations [--name SLUG] [--empty] [--dry-run]` | Diffs compiled models against the migration graph and writes a JSON migration. Never connects to a database. |
| `migrate [TARGET] [--dry-run]` | Applies migrations. |
| `rollback [--steps N \| TARGET] [--dry-run]` | Unapplies migrations. |
| `showmigrations` | `[X]` applied / `[ ]` pending. |
| `squashmigrations FROM TO [--name SLUG]` | Collapses a range. Never connects to a database. |
| `setup [--json]` | Checks Rust prerequisites and prints next steps (standalone binary). |
| `doctor [--json]` | Offline toolchain, project and configuration checks (standalone binary). |
| `completions SHELL` | Prints a bash, zsh or fish completion script (standalone binary). |
| `commands [--json]` | Lists commands with their metadata (standalone binary). |

See [MIGRATIONS.md](MIGRATIONS.md) for the migration commands in detail.

### JSON output

`routes --json` and `check --json` print exactly one JSON value on stdout:
the standard envelope (`schema_version`, `command`, `ok`, `data`,
`diagnostics`). `--json` may come before or after the command. Cargo's build
output stays on stderr.

- `routes`: `data.routes` is a list of `{method, path, operation_id}` sorted
  by path, then method. Hidden endpoints are omitted, as in the text table.
- `check`: `data` holds `issues` (`{level, id, message}`, in text order),
  `errors` and `warnings`. When an error is found, `ok` is `false`, the
  issues are still in `data`, and the exit code is `1`.

Other commands reject `--json` with a usage error (exit `2`). When a command
fails, an error envelope goes to stderr and the exit code is unchanged. The
app factory must not print to stdout, or the output is no longer one JSON
value.

### Global flags

| Flag | Meaning |
|---|---|
| `--database ALIAS` | Alias used by `migrate`, `rollback`, `showmigrations` and `dbshell` (default `default`). |
| `--database-url URL` | Database URL, overriding the settings. |
| `--migrations-dir DIR` | Directory of JSON migrations (default `migrations`). |
| `--json` | One JSON envelope on stdout (`routes`, `check`) |
| `--addr ADDR` | Listen address for `run`. |
| `--help`, `-h` | Help. |

Flags may appear anywhere and take `--flag value` or `--flag=value`.

### Precedence

**Listen address** (`run`): `--addr`, then the `ADDR` environment variable, then `CliSettings::addr`, then `127.0.0.1:8000`. Empty values count as unset.

**Database** (`migrate`, `rollback`, `showmigrations`, `dbshell`): `--database-url`, then the URL configured for the selected alias, then, for the `default` alias only, `DATABASE_URL`. With none of those, the command fails with a "no database" error naming the alias.

## `fmt`, `lint` and `clean`

These run `cargo fmt`, `cargo clippy` and `cargo clean` in the selected
package and return the child's exit code. Arguments after the command are
forwarded unchanged, including `--` boundaries:

```bash
siderite fmt --check
siderite lint --all-targets -- -D warnings
siderite clean
```

`lint` is Rust linting through Clippy. It is not `check`, which validates the
framework configuration, models, migrations and routes. When `rustfmt` or
Clippy is not installed, the command fails with the matching
`rustup component add` hint. `clean` deletes build artifacts only when you
run it; no other command cleans.

## `verify`

`siderite verify` runs the offline checks of the selected package in one
command and prints a per-step summary:

| Step | Runs |
|---|---|
| `fmt` | `cargo fmt --check` |
| `lint` | `cargo clippy --all-targets -- -D warnings` |
| `build` | `cargo build --all-targets` |
| `test` | `cargo test` |
| `check` | `cargo run --quiet -- check --json` (framework checks) |

Every step runs even after an earlier failure, so one pass shows every
problem; only `test` and `check` are skipped when `build` fails. The exit
code is `0` when every step passed, else `1`. `verify` never formats files,
applies Clippy fixes, connects to a database or applies migrations. The only
profile is offline; live checks (connectivity, migration status) are not
available yet.

With `--json`, stdout holds one envelope whose `data` lists each step's
`name`, exact `command`, `status` (`passed`, `failed`, `skipped`),
`duration_ms`, `exit_code` and `message`, plus the `check` report; child
output goes to stderr. A package with several binaries needs `--bin`. The
check step fails, rather than guessing, when the app's stdout is not one
check envelope (for example when the app factory prints).

## `setup` and `doctor`

Both commands are offline and read-only. They never install anything, run
`rustup`, edit shell profiles or connect to a database, and they read no
input, so terminal and scripted runs give the same report. Each check is
`pass`, `warn`, `fail` or `skip`; any `fail` exits with `1`. With `--json`
the report is the `data` of the standard envelope, and `ok` is `false` when
a check failed.

`siderite setup` checks the prerequisites for building any Siderite app:
`rustc` and `cargo` (with their versions compared against this framework's
minimum Rust version), whether `rustup` manages them, and a C linker (Xcode
Command Line Tools on macOS, `cc` elsewhere; not checked on Windows). It
ends with numbered next steps such as the rustup install command,
`rustup update stable` or `xcode-select --install`.

`siderite doctor` runs the same toolchain checks from inside a project,
comparing against the package's `rust-version`, and then checks:

| Check | Fails when | Advisory (`warn`) when |
|---|---|---|
| `project` | `Cargo.toml` is invalid or the package is ambiguous | |
| `config` | `siderite.toml` or `SIDERITE_*` variables do not load | |
| `features.ALIAS` | | a configured backend's feature (`postgres`, `mysql`) is not enabled on a direct siderite dependency |
| `clients.NAME` | | `sqlite3`, `psql` or `mysql` is missing (only `dbshell` needs it) |
| `network.addr` | | the listen address cannot be bound right now |

Database URLs are never printed, including in configuration parse errors.
A free port is only true at the moment of the check, and `--addr` is
honored. `rustc` and `cargo` are queried with `RUSTUP_AUTO_INSTALL=0`, so a
toolchain file never triggers a download; a pinned toolchain that is not
installed is reported with the hint `rustup toolchain install`. Without a
package `rust-version`, doctor compares against the framework's minimum.

## Shell completions

The standalone `siderite` binary prints a completion script for Bash (3.2 or
newer), Zsh or Fish. Scripts are derived from the same command metadata as
`siderite commands --json`, so they cover nested subcommands, fixed argument
values and global flags. The command only writes to standard output; it never
edits a shell profile. Install the script yourself:

```bash
eval "$(siderite completions bash)"                                # current session; add to ~/.bashrc to keep
mkdir -p ~/.zfunc && siderite completions zsh > ~/.zfunc/_siderite     # add fpath=(~/.zfunc $fpath) before compinit
siderite completions fish > ~/.config/fish/completions/siderite.fish
```

An unknown or missing shell name is a usage error (exit `2`). Regenerate the
script after upgrading `siderite`.

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
