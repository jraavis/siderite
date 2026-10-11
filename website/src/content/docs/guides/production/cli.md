---
title: CLI
description: AppCli versus the standalone siderite binary, commands, flags, check, and dbshell.
---

Install the CLI, then use it instead of typing `cargo run -- …`:

```bash
cargo install --path crates/siderite-cli
siderite new myapp                 # or: siderite new myapp --template api
cd myapp
siderite run
```

`new` writes an API crate. In a Cargo package, `run`, `routes`, `check`,
`dbshell`, `makemigrations` and the other migration commands invoke
`cargo run -- <command>` so `AppCli` in your binary sees the app. `build`,
`test`, `fmt`, `lint` and `clean` are `cargo build` / `test` / `fmt` /
`clippy` / `clean` in the package, with every argument after the command
passed through unchanged and the exit status forwarded
(`siderite build --release` replaces `cargo build --release -p myapp`,
`siderite lint -- -D warnings` runs Clippy). `lint` is Rust linting; it
is unrelated to the framework's `check`. A missing `rustfmt` or `clippy`
component is reported with the `rustup component add` command to run. Without a package, `migrate` / `rollback` /
`showmigrations` / `squashmigrations` run against JSON files.

| | `siderite` in a package | Standalone (no package) | `AppCli` in your binary |
|---|---|---|---|
| Needs your `App` and models | compiles them via cargo | no | yes |
| Commands | `new`, `doctor`, `verify`, `run`, `routes`, `check`, `dbshell`, `openapi export`, migrations, `build`, `test`, `fmt`, `lint`, `clean` | `migrate`, `rollback`, `showmigrations`, `squashmigrations` | `run`, `routes`, `check`, `dbshell`, migrations |

Exit codes: `0` success, `1` failure (or `check` found an error), `2` usage
error.

## Standalone binary

```bash
cargo install --path crates/siderite-cli --features postgres,mysql
siderite migrate --database-url postgres://app@localhost/app
siderite showmigrations --migrations-dir db/migrations
```

SQLite is always compiled in. PostgreSQL and MySQL are opt-in cargo
features of `siderite-cli`: `postgres` and `mysql`. The backend is picked by
URL scheme (`sqlite:`, `postgres://`, `mysql://`); a URL for a backend that
was not compiled in is an error. Error messages never contain the URL.
`squashmigrations` only reads and writes files, so it works without a
database URL.

## `AppCli`

```rust
use siderite_cli::{AppCli, CliSettings};
use siderite::prelude::*;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    AppCli::new(build_app)
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

`AppCli::new` takes a factory: `check` builds the app more than once.
`CliSettings` holds the listen address and database URLs by alias. Build it
from loaded configuration with `CliSettings::from(&siderite::config::load()?)`,
which takes `server.addr` and every `databases.<alias>.url`. Its `Debug`
output lists aliases only. `AppCli::run_from(args)` takes explicit
arguments, which is handy in tests.

`run` connects every SQL alias and registers them as the app’s
`Databases`. Two hooks shape that registry:

```rust
AppCli::new(build_app)
    .configure_db(|_alias, db| db.with_signals(receivers::signals()))
    .database_router(AppRouter)
```

`configure_db` runs once per connected alias, before registration.
`database_router` installs a [DatabaseRouter](/siderite/guides/data/database-routing/).

## Commands

| Command | What it does |
|---|---|
| `run [--addr ADDR]` | Connects every configured SQL database, registers each under its alias, and serves the app. Redis and MongoDB aliases are skipped: register those yourself in the factory |
| `routes [--json]` | Prints `METHOD PATH operation_id` for every route, mounts included |
| `check [--json]` | Validates configuration, models, migrations, routes, and the backend. Exits `1` when an error is found |
| `dbshell` | Starts the database’s native client |
| `openapi export [--output PATH] [--force]` | Writes the OpenAPI 3.1 document to stdout or `PATH`; no database, server or migration |
| `makemigrations [--name SLUG] [--empty] [--dry-run]` | Diffs compiled models against the graph and writes a JSON migration. Never connects to a database |
| `migrate [TARGET] [--dry-run]` | Applies migrations |
| `rollback [--steps N \| TARGET] [--dry-run]` | Unapplies migrations |
| `showmigrations [--json]` | `[X]` applied / `[ ]` pending |
| `squashmigrations FROM TO [--name SLUG]` | Collapses a range. Never connects to a database |

See [Migrations](/siderite/guides/data/migrations/) for the file format and
reversibility.

### Global flags

| Flag | Meaning |
|---|---|
| `--database ALIAS` | Alias used by `migrate`, `rollback`, `showmigrations`, and `dbshell` (default `default`) |
| `--database-url URL` | Database URL, overriding the settings |
| `--migrations-dir DIR` | Directory of JSON migrations (default `migrations`) |
| `--addr ADDR` | Listen address for `run` |
| `--json` | One JSON report on stdout (`check`, `routes`; see below) |
| `--help`, `-h` | Help |

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

**Listen address** (`run`): `--addr`, then the `ADDR` environment
variable, then `CliSettings::addr`, then `127.0.0.1:8000`. Empty values
count as unset.

**Database** (`migrate`, `rollback`, `showmigrations`, `dbshell`):
`--database-url`, then the URL configured for the selected alias, then,
for the `default` alias only, `DATABASE_URL`. With none of those, the
command fails with a “no database” error naming the alias.

## Development loop

```bash
siderite dev                       # build, run, r+Enter rebuilds
siderite dev -- --addr 0.0.0.0:8000
```

`dev` builds the app, runs it, and rebuilds when a file the binary was
compiled from changes (taken from cargo's dep-info, so path dependencies
and `include_str!` files count), or `Cargo.toml`, `Cargo.lock`,
`siderite.toml` or `migrations/`. Files the app writes at run time never
trigger a rebuild, and editing a migration never applies it. A failed
build keeps the previous version serving; a successful one stops the old process (SIGTERM, then SIGKILL
after `--grace-ms`, default 5 s, including anything it started) and waits
for its port to be free before starting the new one. Ctrl-C cancels a
build and stops the app. This restarts the process; it is not hot reload.

## Generators

```bash
siderite generate route items             # src/routes/items.rs
siderite generate route items --dry-run   # show it only
siderite generate model book title:String pages:i32? --table books
siderite generate resource book title:String --table books
siderite generate test resource book title:String --table books
```

A generated route module has one documented handler (`GET /items`,
operation ID `items_index`), its response type and a `routes()` function.
siderite never edits your existing code; it prints the two lines that
register the module. A generated model has an `id` primary key and
fields of type `i32`, `i64`, `f64`, `bool` or `String` (`?` for nullable);
register its `META` with `AppCli::models`, then run `makemigrations`.
`generate resource` adds a paged list (ordered by `id`, at most 100 per
page, always two queries), create, get, replace (`PUT`), partial update
(`PATCH`: absent fields stay, `null` clears nullable ones) and delete
handlers with validated bodies, 404s for missing rows and an `id` that
never changes; they have no authentication until you add it.
Existing files are never overwritten.

`generate test route NAME` and `generate test resource NAME ...` (the
same arguments as the generator) write `tests/NAME.rs` with
`siderite-testkit`: status codes, content type, the documented operation,
a create-then-fetch round trip, and failure cases (a `405`, rejected
bodies, a `404`). A resource test creates a throwaway in-memory SQLite
database from the model, so your configured databases are never used.

### CI workflow

```bash
siderite generate ci --dry-run   # .github/workflows/siderite.yml
siderite generate ci
```

A GitHub Actions workflow that runs `siderite verify` on every push and
pull request and keeps the JSON report as an artifact. A separate `live`
job runs `siderite verify --live` only by hand or on `main`, with
PostgreSQL or MySQL service containers when the CLI is built with those
features. The Rust version is pinned (toolchain file, `rust-version`, or
the `rustc` you use now) and the CLI is installed exactly as
`Cargo.lock` pins it. The workflow holds no secrets. An existing workflow
is never overwritten; `--update` replaces only one this command wrote.

## Dependencies

```bash
siderite add serde --features derive   # cargo add for the app package
siderite add --preset postgres         # enable PostgreSQL support
siderite add --preset mysql --dry-run  # show the change only
siderite remove serde
```

Cargo makes every manifest edit, so `workspace = true` and path or git
sources stay as they are. A preset only adds a feature to the existing
`siderite-cli` dependency. `remove --preset` changes nothing and tells you
which feature to delete by hand, so features you added yourself survive.

## Setup

```bash
siderite setup             # report only
siderite setup --install   # also run the rustup steps it suggests
```

Checks rustup, `rustc` (against siderite's minimum Rust version),
`cargo`, `rustfmt`, `clippy` and a C linker, and prints the exact command
to fix each gap for your operating system. `--install` runs only rustup
commands (toolchain and components) and never edits shell profiles or
installs system packages.

## Shell completions

```bash
siderite completions bash > ~/.local/share/bash-completion/completions/siderite
siderite completions fish > ~/.config/fish/completions/siderite.fish
siderite completions zsh > "${fpath[1]}/_siderite"
```

Generated from the same command list as the help, so they never drift.
Your shell profile is never edited.

## Local services

```bash
siderite services up            # PostgreSQL in a container, ready to use
export "$(siderite services env)"   # DATABASE_URL for this shell
siderite services down          # stop it; the data stays
siderite services destroy postgres --yes   # delete container and data
```

Optional, and only with a container runtime you installed: Docker or
Podman on `PATH`, or the program named by `SIDERITE_CONTAINER_RUNTIME`.
Each project gets its own container and data volume, named after the
package directory and a random id. The port is published on `127.0.0.1`
only, and the password is generated; both are kept in `.siderite/local/`,
which git ignores. `up` waits until the database accepts connections
(`--timeout`, default 60 s). Reports never show the password;
`services env` prints it, because that is what it is for.

## Doctor

```bash
siderite doctor          # or: siderite doctor --json
```

Checks, without building the app or touching a database: `rustc` and
`cargo` are present and at least the Rust version siderite and your
package need; `rustfmt` and `clippy` are installed; `siderite.toml` and
`SIDERITE_*` variables load; each database alias uses a supported scheme
that this `siderite` binary was built for; and whether the listen address
is free (advisory). Missing `psql`/`mysql`/`sqlite3` clients are reported
for information only. Every warning or error comes with a fix hint; the
exit code is `1` when a check fails.

`siderite doctor --live [--database ALIAS]... [--timeout-ms N]` also
connects to the configured databases (all of them, or the ones named),
read-only, and reads each one's migration status: at most four at a
time, each within the timeout (default 5 s). Only the requested aliases
can fail, errors never show URLs, SQLite files are opened read-only and
never created, and Redis/MongoDB aliases are not probed.

## Verify

```bash
siderite verify          # fmt, lint, build, test and check
siderite verify --live   # also the ignored (live-service) tests
```

One report for the whole local verification: each step's status, time
and, on failure, the redacted end of its output. A failed build skips the
tests and the framework check; files are never reformatted and migrations
are never applied. `--json` writes one `verify` envelope; compiler errors
and Clippy findings appear there with their codes, file spans and rendered
explanations.

## Configuration

```bash
siderite config show             # every value and its source, redacted
siderite config validate --json  # load it and check database URLs
```

Both read what `siderite_config::load` reads from the package
directory: `siderite.toml`, `SIDERITE_*` variables and the `DATABASE_URL`
and `ADDR` aliases. `show` prints each resolved key with the source that
won: a default, the file, the variable's name or an override. Secrets are
always redacted (`--redacted` only says so): secret keys show `***`, URLs
lose their passwords. `validate` exits `1` when the settings do not load
(`config.E004`) or a database URL is unusable (`config.E002`,
`config.E003`).

## Docs search

```bash
siderite docs search queryset filter
siderite docs search transaction --json --limit 3
```

Searches the documentation of the installed CLI's version without a
network connection. Results name the source file and line, link to it at
the release tag and list the Cargo features the section mentions. If your
project's `Cargo.lock` uses another siderite version, a warning says so.

## Explain

```bash
siderite explain models.E003     # cause, fix, verification command
siderite explain E0308           # points to rustc --explain
siderite explain --list          # every siderite code
```

Every diagnostic code siderite reports has an entry: why it appears, the
smallest correction and the command that shows it is fixed, for this
version. Compiler codes and Clippy lints point to their official pages
instead of a siderite explanation; an unknown code exits `1`.

## AI context

```bash
siderite ai dump-context            # Markdown
siderite ai dump-context --json     # versioned JSON envelope
siderite ai dump-context --live     # also read migration status
```

`siderite ai init` writes `.siderite/context.md` for AI assistants:
commands, conventions, this project's models and routes, and examples
compiled against your siderite version. Rerun it after changes (`--check`
tells CI when it is stale). Only the block between its markers is
generated; your notes around it are kept, and a hand-edited block is
left alone unless you pass `--force`.

`ai dump-context` describes the app from its compiled metadata: framework
version, database aliases and backend kinds (never URLs), models and
their fields, documented routes, and migration status (`unknown` unless
`--live`). Output is bounded (`--max-bytes`), says what was cut, and is
redacted. It names the binary and its build time so stale context can be
recognised.

For one task, `siderite ai context --task "add a tag filter to posts"`
picks the matching models and routes, the relevant documentation and
examples for your siderite version, and any files you name with
`--include`, within a byte budget (`--budget`, default 32768). It says
what it left out and what it could not know. Included files must be in
the app package; git-ignored and secret-looking files are skipped.

Before handing work back, `siderite ai validate` runs the same steps as
`siderite verify` and lists each failure with its file and line, the fix
`siderite explain` gives for its code, a documentation link and the
command that reproduces it. It changes nothing, and a clean report only
means these checks passed: application logic still needs its own tests.

## JSON output

`check`, `docs search`, `doctor`, `explain`, `routes`, `showmigrations` and `verify` accept `--json`, before or after the command
(`siderite --json routes`, `siderite check --json`). Stdout then carries
exactly one JSON value and a newline, a versioned envelope described by
[`docs/schemas/cli-report-v1.json`](https://github.com/jraavis/siderite/blob/master/docs/schemas/cli-report-v1.json):

```json
{"schema_version":1,"command":"routes","ok":true,"data":{"routes":[]},"diagnostics":[]}
```

- Exit codes keep their meaning: `0` success, `1` failure (`check` found an
  error, `ok: false`), `2` usage error.
- Every failure is an envelope too, with `data: null`, `ok: false` and one
  diagnostic: `cli.E001` usage error, `cli.E002` the command failed,
  `cli.E003` the application exited without a report (build failure,
  crash, or a `siderite-cli` that predates `--json`).
- Through `siderite`, cargo's build output and anything the app factory
  prints go to stderr: the app writes its report to a private file
  (`SIDERITE_REPORT_PATH`), never to the shared stdout.
- `--json` with any other command, with `--help`, or without a command is
  a usage error. `routes` lists documented routes only (hidden endpoints
  are omitted).
- `showmigrations --json` reports the selected alias, where its URL came
  from (never the URL), and each migration's `id` and `applied` flag in
  the text order. It is a read-only live query bounded to 30 seconds; a
  missing database or unreadable history is a failure, never an empty
  list.

## OpenAPI export

```bash
siderite openapi export > openapi.json
siderite openapi export --output openapi.json --force
```

The document is the one `/openapi.json` serves (`App::openapi()`),
pretty-printed and deterministic, and export works with the HTTP docs
disabled. Only the app factory runs, so keep it free of side effects. A
relative `--output` is resolved against your current directory; the file
is written atomically and an existing file is replaced only with `--force`.

## Release packages

```toml
# Cargo.toml
[package.metadata.siderite.package]
assets = ["static"]        # migrations/ is included when it exists
```

```bash
siderite build                  # once, to create Cargo.lock; commit it
siderite package --release      # target/siderite-package/NAME-VERSION-HOST
```

The package is a directory with the release binary (`bin/NAME`), the
declared assets, the migrations, `manifest.json` (versions, target,
`rustc`, every file's size and SHA-256, the shared libraries the binary
needs) and `SHA256SUMS` for `sha256sum -c`. The build uses
`--locked`, so it uses exactly the versions in `Cargo.lock`. `.env*`
files, databases, keys, `.git*`, `target` and `.siderite` are always left
out, and a declared file that holds a credential stops packaging. The
binary is run once with an empty `PATH` to show it works without Cargo.
Nothing is deployed and no migration is applied.

## Comparing OpenAPI documents

```bash
siderite openapi export --output openapi.json   # on the old version
siderite openapi diff openapi.json new.json     # exit 1 on breaking
```

`openapi diff BASE CURRENT` reads two local JSON documents and lists what
changed for existing clients. Changes are breaking (removed operations,
newly required inputs, narrower request schemas, response properties that
disappear), unknown (things it does not compare) or additive. Each change
names its JSON pointer in either document. Only `#/` references are
followed; nothing is fetched. Unknown counts as incompatible, so the exit
status is `1` for breaking or unknown changes, which suits a CI check.

## `check`

`check` runs without starting a server or opening a database. Each issue
prints as `error: [models.E003] message` or `warning: [id] message`.
Messages never contain database URLs.

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
| `openapi.E001` | error | the OpenAPI document cannot be generated |
| `routes.E001` | error | the app cannot be built into a router |
| `backend.E001` | error | the `default` database is Redis, which cannot hold models |
| `backend.E002` | error | a model needs joins the `default` backend does not support |
| `backend.E003` | error | the schema cannot be created on the `default` backend |
| `backend.W001` | warning | the backend has no schema migrations; `migrate` will refuse to run |

Migration checks are skipped when no migrations directory is passed to
`siderite_cli::check`; `AppCli` always passes its own.

## Seed datasets

```rust
AppCli::new(build_app)
    .seed("demo", "A user with two posts", |s| async move {
        // s.db is the selected database, inside one transaction
        demo_data(&s.db).await?;
        Ok(())
    })
```

```bash
siderite seed --list
siderite seed --dataset demo                # the `default` database
siderite --database analytics seed --dataset demo
```

A dataset runs only when you name it: never on startup, after
migrations or by default. The report says which dataset went into which
database alias, with the URL's password removed. The whole dataset runs in
one transaction (SQLite, PostgreSQL and MySQL), so a failure
(`seed.E001`) leaves nothing behind. Seeding never deletes or resets
data, never reads SQL files, and is not a migration: nothing is recorded
and there is nothing to roll back. Write datasets so a second run is
harmless, for example by looking rows up before inserting them.

## `dbshell`

`dbshell` starts `sqlite3`, `psql`, or `mysql` on the selected database and
returns the client’s exit code. Passwords never appear on the command line
or in output: `psql` receives `PGPASSWORD` and `mysql` receives `MYSQL_PWD`
in its environment. A userinfo password and a PostgreSQL `?password=` query
parameter both move into `PGPASSWORD` and are stripped from the URL passed
to `psql`. A URL that carries `sslpassword` is refused. For MySQL only
host, port, user, and database are forwarded. An in-memory SQLite URL, an
unknown scheme, or a client that is not on `PATH` is an error. MongoDB and
Redis are not supported.

## See also

- [Migrations](/siderite/guides/data/migrations/)
- [Configuration](/siderite/guides/production/config/)
- [Signals](/siderite/guides/data/signals/)
