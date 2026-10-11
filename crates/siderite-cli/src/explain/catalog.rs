//! Every stable diagnostic code siderite emits, with its cause, the
//! smallest correction and the command that confirms it. A test checks
//! that this table and the codes in the source stay in step.

/// One code's explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Explanation {
    /// The code, `namespace.E001`.
    pub code: &'static str,
    /// One line.
    pub title: &'static str,
    /// The first siderite version that emits it.
    pub since: &'static str,
    /// Why it is reported.
    pub cause: &'static str,
    /// The minimal valid correction.
    pub fix: &'static str,
    /// The command that shows it is fixed.
    pub verify: &'static str,
    /// The docs section about it (`siderite docs search` id).
    pub docs: &'static str,
}

const CHECK: &str = "guides/production/cli#check";
const CONFIG: &str = "guides/production/cli#configuration";
const OPENAPI_DIFF: &str = "guides/production/cli#comparing-openapi-documents";
const SEED: &str = "guides/production/cli#seed-datasets";
const SERVICES: &str = "guides/production/cli#local-services";
const DOCTOR: &str = "guides/production/cli#doctor";
const JSON: &str = "guides/production/cli#json-output";

const fn entry(
    code: &'static str,
    title: &'static str,
    texts: [&'static str; 2],
    verify: &'static str,
    docs: &'static str,
) -> Explanation {
    let [cause, fix] = texts;
    Explanation {
        code,
        title,
        since: "0.1.0",
        cause,
        fix,
        verify,
        docs,
    }
}

/// The catalog, sorted by code.
pub(crate) const CATALOG: &[Explanation] = &[
    entry(
        "ai.E001",
        "Live migration status could not be read",
        [
            "`ai dump-context --live` asked for the migration status, but \
             the database could not be reached or read.",
            "Start the database and check the alias's URL, or drop --live \
             to get the offline context.",
        ],
        "siderite showmigrations --json",
        "guides/production/cli#ai-context",
    ),
    entry(
        "backend.E001",
        "Models on a key/value store",
        [
            "The `default` database is Redis, which cannot store models.",
            "Point `default` at SQLite, PostgreSQL, MySQL or MongoDB and \
             keep Redis under another alias for caching.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "backend.E002",
        "Relations the backend cannot join",
        [
            "A model has a foreign key or many-to-many relation, but the \
             `default` backend has no joins (MongoDB), or the initial \
             schema could not be diffed.",
            "Store related data on a SQL backend, or replace the relation \
             with an id field and look it up explicitly.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "backend.E003",
        "Schema the backend cannot create",
        [
            "The initial schema uses something the `default` backend cannot \
             express, so `migrate` would fail.",
            "Change the field type or option named in the message to one \
             the backend supports (see the backend matrix).",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "backend.W001",
        "Backend without schema migrations",
        [
            "The `default` backend (MongoDB) has no schema migrations, so \
             `migrate` refuses to run.",
            "Nothing to fix if that is intended; create collections and \
             indexes in your own setup code.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "cli.E001",
        "Usage error",
        [
            "The command line was not valid: an unknown flag, a missing \
             value or a flag that does not apply to the command.",
            "Fix the arguments as the message says; `siderite help` lists \
             each command's usage.",
        ],
        "siderite commands",
        JSON,
    ),
    entry(
        "cli.E002",
        "Command failed",
        [
            "The command started but failed: a connection, IO, migration \
             or configuration error, named in the message.",
            "Fix the cause in the message; rerun with the same arguments.",
        ],
        "siderite doctor",
        JSON,
    ),
    entry(
        "cli.E003",
        "The application wrote no report",
        [
            "`siderite` ran the app through `cargo run`, but the app exited \
             without writing its JSON report, usually because it failed \
             before its CLI ran or uses an older siderite-cli.",
            "Read the app's stderr; make `main` call `AppCli::run` and use \
             a siderite-cli version that matches the siderite CLI.",
        ],
        "siderite check --json",
        JSON,
    ),
    entry(
        "config.E001",
        "Models without a default database",
        [
            "Managed models are registered but no `default` database is \
             configured.",
            "Set `databases.default.url` in siderite.toml, or DATABASE_URL.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "config.E002",
        "Malformed database URL",
        [
            "A database URL has no scheme or does not parse as a URL.",
            "Write it as `scheme://user:password@host:port/db` (SQLite: \
             `sqlite://path.db` or `sqlite::memory:`).",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "config.E003",
        "Unsupported database scheme",
        [
            "A database URL uses a scheme siderite has no backend for.",
            "Use sqlite, postgres, mysql, mongodb or redis.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "config.E004",
        "Configuration does not load",
        [
            "siderite.toml, a SIDERITE_* variable or an alias \
             (DATABASE_URL, ADDR) has a value the settings cannot take.",
            "Fix the key named in the message; `siderite config show` \
             names the source of each value.",
        ],
        "siderite config validate",
        CONFIG,
    ),
    entry(
        "docs.W001",
        "Docs for another siderite version",
        [
            "The project's Cargo.lock has a siderite version other than the \
             one the CLI's docs describe.",
            "Install the CLI matching the project (`cargo install \
             siderite-cli --version X`), or upgrade the project.",
        ],
        "siderite docs search QUERY",
        "guides/production/cli#docs-search",
    ),
    entry(
        "doctor.E001",
        "rustc not found",
        [
            "`rustc` was not on PATH or did not run.",
            "Install Rust with rustup (https://rustup.rs) and put \
             ~/.cargo/bin on PATH; `siderite setup --install` runs the \
             rustup steps when rustup exists.",
        ],
        "siderite doctor",
        DOCTOR,
    ),
    entry(
        "doctor.E002",
        "cargo not found",
        [
            "`cargo` was not on PATH or did not run.",
            "Install Rust with rustup and put ~/.cargo/bin on PATH.",
        ],
        "siderite doctor",
        DOCTOR,
    ),
    entry(
        "doctor.E003",
        "rustc older than siderite needs",
        [
            "The active `rustc` is older than siderite's minimum Rust \
             version.",
            "Run `rustup update`, or check `rust-toolchain.toml` pins a new \
             enough toolchain.",
        ],
        "siderite doctor",
        DOCTOR,
    ),
    entry(
        "doctor.E004",
        "rustc older than the project's rust-version",
        [
            "The active `rustc` is older than the `rust-version` in the \
             project's Cargo.toml.",
            "Run `rustup update`, or lower `rust-version` if the project \
             really supports older compilers.",
        ],
        "siderite doctor",
        DOCTOR,
    ),
    entry(
        "doctor.E005",
        "Configuration does not load",
        [
            "siderite.toml or a SIDERITE_* variable could not be read or \
             parsed.",
            "Fix the file or variable named in the message.",
        ],
        "siderite doctor",
        DOCTOR,
    ),
    entry(
        "doctor.E006",
        "Unsupported database scheme",
        [
            "A configured database alias has no URL scheme or an unknown \
             one.",
            "Use sqlite:, postgres://, mysql://, mongodb:// or redis://.",
        ],
        "siderite doctor",
        DOCTOR,
    ),
    entry(
        "doctor.E007",
        "Live database probe failed",
        [
            "`doctor --live` could not connect to an alias or read its \
             migration status: the server is down, unreachable, rejects \
             the credentials, timed out, or a SQLite file is missing.",
            "Start the server, check the alias's host and credentials, or \
             run `siderite migrate` for a new SQLite file.",
        ],
        "siderite doctor --live",
        "guides/production/cli#doctor",
    ),
    entry(
        "doctor.E008",
        "Unknown database alias",
        [
            "`doctor --live --database ALIAS` named an alias that is not \
             configured.",
            "Configure the alias in siderite.toml or SIDERITE_DATABASES__*, \
             or name a configured one.",
        ],
        "siderite doctor --live",
        DOCTOR,
    ),
    entry(
        "doctor.W001",
        "rustfmt not installed",
        [
            "The toolchain has no rustfmt, so `siderite fmt` and `verify` \
             cannot check formatting.",
            "Run `rustup component add rustfmt`.",
        ],
        "siderite doctor",
        DOCTOR,
    ),
    entry(
        "doctor.W002",
        "Clippy not installed",
        [
            "The toolchain has no Clippy, so `siderite lint` and `verify` \
             cannot lint.",
            "Run `rustup component add clippy`.",
        ],
        "siderite doctor",
        DOCTOR,
    ),
    entry(
        "doctor.W003",
        "Backend missing from this siderite binary",
        [
            "An alias uses PostgreSQL or MySQL, but this `siderite` binary \
             was built without that feature, so standalone `migrate` \
             cannot use it. The app binary is not affected.",
            "Reinstall with `cargo install siderite-cli --features \
             postgres` (or `mysql`).",
        ],
        "siderite doctor",
        DOCTOR,
    ),
    entry(
        "doctor.W004",
        "Server address in use",
        [
            "The address `run` would bind is in use or cannot be bound now.",
            "Stop the other process, or choose another address with `run \
             --addr` or ADDR.",
        ],
        "siderite doctor",
        DOCTOR,
    ),
    entry(
        "doctor.W005",
        "Application package not chosen",
        [
            "The project could not be resolved to one application package \
             (several candidates, or cargo metadata failed).",
            "Pass --manifest-path or --package to choose the app.",
        ],
        "siderite doctor",
        DOCTOR,
    ),
    entry(
        "doctor.W006",
        "Unrecognised rustc version",
        [
            "`rustc --version` printed something doctor cannot parse.",
            "Run `rustc --version` and check the toolchain or wrapper on \
             PATH.",
        ],
        "siderite doctor",
        DOCTOR,
    ),
    entry(
        "explain.E001",
        "Unknown diagnostic code",
        [
            "`siderite explain` was given a code that is neither a \
             siderite code nor a rustc or Clippy code.",
            "Copy the code exactly from the report; `siderite explain \
             --list` shows every siderite code.",
        ],
        "siderite explain --list",
        "guides/production/cli#explain",
    ),
    entry(
        "migrations.E001",
        "Migration files do not load",
        [
            "A file in the migrations directory cannot be read or is not \
             valid migration JSON.",
            "Fix or remove the file named in the message; never edit a \
             migration that has been applied.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "migrations.E002",
        "Invalid migration graph",
        [
            "The migrations' dependencies name a missing migration, form a \
             cycle, or leave several heads (unmerged branches).",
            "Fix the `dependencies` named in the message; for several \
             heads, make the newer migration depend on the other head.",
        ],
        "siderite showmigrations",
        CHECK,
    ),
    entry(
        "migrations.E003",
        "Migrations do not replay",
        [
            "Applying the migrations in order to an empty project state \
             fails (for example altering a field that does not exist).",
            "Fix the operation named in the message in an unapplied \
             migration, or add a new migration that repairs the state.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "migrations.E004",
        "Model changes makemigrations would refuse",
        [
            "The difference between the migrations and the models cannot \
             be written as a migration (for example an ambiguous change).",
            "Split the model change into smaller steps, running \
             `makemigrations` after each.",
        ],
        "siderite makemigrations --dry-run",
        CHECK,
    ),
    entry(
        "migrations.W001",
        "Model changes without a migration",
        [
            "The models differ from the state the migrations record.",
            "Run `siderite makemigrations`, review the new file, then \
             `siderite migrate`.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "models.E001",
        "Table used twice",
        [
            "Two models (or a model and a many-to-many join table) use the \
             same table name.",
            "Give one of them another `#[model(table = \"...\")]`.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "models.E002",
        "Model registered twice",
        [
            "The same model name appears more than once in \
             `AppCli::models`.",
            "Register each model once.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "models.E003",
        "Model without a primary key",
        [
            "A model has no primary-key field.",
            "Add an `id` field marked `#[field(primary_key, auto)]`.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "models.E004",
        "Foreign key to an unregistered model",
        [
            "A relation field targets a model missing from \
             `AppCli::models`.",
            "Register the target model.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "models.E005",
        "Many-to-many with an unregistered model",
        [
            "A many-to-many relation targets, or goes through, a model \
             missing from `AppCli::models`.",
            "Register the target and the `through` model.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "models.E006",
        "Column mapped twice",
        [
            "Two fields of one model map to the same column.",
            "Rename one field or its `#[field(column = \"...\")]`.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "models.E007",
        "Several primary keys",
        [
            "A model marks more than one field as primary key; composite \
             keys are not supported.",
            "Keep one primary key and enforce the combination with a \
             unique constraint.",
        ],
        "siderite check",
        CHECK,
    ),
    entry(
        "openapi.E001",
        "OpenAPI document cannot be generated",
        [
            "Two routes share an operation id, or a schema cannot be \
             described.",
            "Give each route a unique operation id (function name or \
             `operation_id`), as the message names.",
        ],
        "siderite openapi export",
        CHECK,
    ),
    entry(
        "openapi.E002",
        "Breaking API changes",
        [
            "`openapi diff` found changes that can break existing \
             clients: removed operations or responses, newly required \
             inputs, or narrower or changed schemas.",
            "Keep the old contract (add, do not remove or require), or \
             version the API and tell clients; each change names its \
             JSON pointer.",
        ],
        "siderite openapi diff BASE CURRENT",
        OPENAPI_DIFF,
    ),
    entry(
        "openapi.E003",
        "API changes not compared",
        [
            "`openapi diff` met something it does not compare: external \
             references, composed schemas (oneOf, anyOf, allOf), security \
             or callbacks. Unknown is treated as incompatible.",
            "Review each listed change by hand; prefer local `#/` \
             references so the comparison can follow them.",
        ],
        "siderite openapi diff BASE CURRENT",
        OPENAPI_DIFF,
    ),
    entry(
        "routes.E001",
        "App cannot be built",
        [
            "The router cannot be assembled: two handlers for one method \
             and path, or a route, mount or docs path not starting with \
             `/`.",
            "Change the route named in the message.",
        ],
        "siderite routes",
        CHECK,
    ),
    entry(
        "seed.E001",
        "Seed dataset failed",
        [
            "The dataset's callback returned an error. Where the backend \
             has transactions (SQLite, PostgreSQL, MySQL) nothing it wrote \
             was kept.",
            "Fix the dataset or the data it expects, then run it again; \
             datasets should be safe to run twice.",
        ],
        "siderite seed --dataset NAME",
        SEED,
    ),
    entry(
        "services.E001",
        "No container runtime",
        [
            "`siderite services` runs local services with Docker or \
             Podman, and neither was found on PATH.",
            "Install Docker or Podman, or set SIDERITE_CONTAINER_RUNTIME \
             to the runtime program.",
        ],
        "siderite services status",
        SERVICES,
    ),
    entry(
        "services.E002",
        "Container runtime command failed",
        [
            "The runtime refused a command (daemon not running, port in \
             use, image not available) or the container stopped while \
             starting.",
            "Start the runtime's daemon, free or change the port \
             (`--port`), or read the container's logs.",
        ],
        "siderite services up",
        SERVICES,
    ),
    entry(
        "services.E003",
        "Service not ready in time",
        [
            "The database did not accept connections within --timeout \
             (the first start pulls the image and initializes data).",
            "Run again with a longer --timeout, or read the container's \
             logs.",
        ],
        "siderite services up --timeout 180",
        SERVICES,
    ),
    entry(
        "setup.E001",
        "No C linker",
        [
            "No `cc`, `gcc` or `clang` is on PATH, and rustc links through \
             the platform's C toolchain.",
            "Install one: `xcode-select --install` (macOS), `sudo apt install \
             build-essential` (Debian, Ubuntu) or `sudo dnf install gcc` \
             (Fedora).",
        ],
        "siderite setup",
        "guides/production/cli#setup",
    ),
    entry(
        "snapshot.E001",
        "Snapshot without the OpenAPI document",
        [
            "The app snapshot (`ai dump-context`) could not generate the \
             OpenAPI document, so routes are missing from it.",
            "Fix the error `siderite check` reports as openapi.E001.",
        ],
        "siderite check",
        "guides/production/cli#ai-context",
    ),
    entry(
        "snapshot.W001",
        "Partial snapshot",
        [
            "The snapshot exceeded a size or count limit, so some models, \
             routes or the OpenAPI document were left out.",
            "Raise the limit (`--max-bytes`), or read the parts listed in \
             `completeness` with `siderite routes` or `openapi export`.",
        ],
        "siderite ai dump-context --json",
        "guides/production/cli#ai-context",
    ),
    entry(
        "verify.E001",
        "A verification step failed",
        [
            "One of fmt, lint, build, test or check failed in `siderite \
             verify`.",
            "Run the step's command from the report and fix what it \
             reports; `cargo fmt` fixes formatting.",
        ],
        "siderite verify",
        "guides/production/cli#verify",
    ),
];
