# Configuration

`siderite::config` (crate `siderite-config`) loads `Settings` with
[figment](https://docs.rs/figment).

## Sources and precedence

Lowest to highest:

1. compiled defaults;
2. TOML files (`file` for a required file, `file_optional` for an optional one);
3. the environment;
4. programmatic overrides (`set`).

Overrides always win, whatever order the builder calls come in. Files and
environment sources merge in the order they are added.

```rust
use siderite::config::{ConfigBuilder, load};

// siderite.toml (optional) + SIDERITE_* + DATABASE_URL / ADDR
let settings = load()?;

let settings = ConfigBuilder::new()
    .file("config/app.toml")
    .env_prefix("MYAPP_")
    .set("app.debug", true)
    .build()?;
```

Environment keys use `__` for nesting once the prefix is stripped:
`SIDERITE_DATABASES__ANALYTICS__URL` sets `databases.analytics.url`.
`env_prefix` also maps two unprefixed aliases, and prefixed keys win over them:

| Variable | Key |
|---|---|
| `DATABASE_URL` | `databases.default.url` |
| `ADDR` | `server.addr` |

`extract::<T>()` deserializes the same merged tree into your own type, so
application-specific sections can live next to the built-in ones.

## `Settings`

```toml
secret_key = "change-me"

[app]
name = "blog"          # default "siderite"
debug = false

[server]
addr = "0.0.0.0:8080"  # default "127.0.0.1:8000"

[databases.default]
url = "postgres://app:pw@db/app"
max_connections = 10

[databases.analytics]
url = "sqlite://analytics.db"

[cache]
url = "redis://cache:6379/0"
max_entries = 1024

[log]
level = "info"
json = false
```

## Secrets

Database URLs, `cache.url` and `secret_key` are `Secret<String>`.

- `Debug` and `Display` print `[REDACTED]`. Call `expose()` to read the value.
- There is no `Serialize` impl, so a secret cannot be written back out by accident.
- `ConfigError` messages never include the value of a secret-looking key (`url`, `password`, `token`, `secret_key`, `api_key`, ...).

## Explaining the configuration

`ConfigBuilder::explain()` returns every resolved value as a `ConfigEntry` (`key`, redacted `value`, `source`), sorted by key, from the same merged sources as `build()`. A source is a compiled default, a file (with its path), an environment variable (with its name, `SIDERITE_SERVER__ADDR` or an alias such as `DATABASE_URL`) or a programmatic override. `siderite config show` prints it; `siderite config validate` loads the settings and checks the database URLs.

## Tracing and the CLI

- `init_tracing(&settings.log)` installs a subscriber. `RUST_LOG` overrides
  `log.level`, and `log.json` selects JSON output. See
  [OBSERVABILITY.md](OBSERVABILITY.md).
- `CliSettings::from(&settings)` hands the address and database URLs to
  `AppCli` ([CLI.md](CLI.md)).
