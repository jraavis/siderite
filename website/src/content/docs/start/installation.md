---
title: Installation
description: Add siderite to a Rust project, pick cargo features, and set the MSRV.
---

siderite is a Cargo workspace. Until the first crates.io release, depend on the
git repository (or a path checkout).

```toml
[dependencies]
siderite = { git = "https://github.com/jraavis/siderite" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The `siderite` crate is the public facade. Most applications depend only on it
and `use siderite::prelude::*;`.

## Toolchain

| Requirement | Value |
|---|---|
| Rust edition | 2024 |
| MSRV | 1.99 |
| Async runtime | Tokio |

```bash
rustup toolchain install 1.99
```

## Cargo features

SQLite is always available. Other backends are opt-in on `siderite-backends`
(re-exported through the workspace). Enable them on the crate that opens the
connection — typically your binary, an example, or `siderite-cli`.

| Feature | What it unlocks | Live-test variable |
|---|---|---|
| *(default)* | SQLite `QuerySet` backend | none |
| `postgres` | PostgreSQL | `DATABASE_URL=postgres://...` |
| `mysql` | MySQL 8 | `MYSQL_URL=mysql://...` |
| `mongodb` | MongoDB (supported QuerySet subset) | `MONGODB_URL=mongodb://...` |
| `redis` | Redis key/hash/set client and `RedisCache` | `REDIS_URL=redis://...` |

A URL for a backend that was not compiled in is an error. Unsupported QuerySet
features fail with a `BackendCapabilityError` before any I/O. Redis is a typed
client, not a QuerySet backend.

See [Backends](/siderite/guides/data/backends/) for the feature matrix.

## Workspace crates

| Crate | Role |
|---|---|
| `siderite` | Facade and prelude |
| `siderite-core` | App, routing, extractors, RFC 7807 errors |
| `siderite-validation` | `Validate`, rules, constrained types, `Schema` |
| `siderite-orm` | `Model`, `QuerySet`, relations, transactions |
| `siderite-backends` | SQLite, PostgreSQL, MySQL, MongoDB, Redis |
| `siderite-macros` | Route attributes, `routes![]`, derives |
| `siderite-openapi` | OpenAPI 3.1 document and UIs |
| `siderite-migrations` | Autodetector, JSON migrations, schema editor |
| `siderite-config` | Layered settings, `Secret`, tracing |
| `siderite-cache` | Memory/Redis cache, `RouteCache` |
| `siderite-cli` | `AppCli` and the standalone `siderite` binary |
| `siderite-testkit` | In-process `TestClient` and `TestDatabase` |

The full map and dependency rules live in [Crate map](/siderite/reference/crates/).

## Clone and run the examples

```bash
git clone https://github.com/jraavis/siderite
cd siderite
cargo install --path crates/siderite-cli
cd examples/hello_world
siderite run
```

`cargo run -p hello_world -- run` is the same thing without installing the CLI.

Open [http://127.0.0.1:8000/hello/ann](http://127.0.0.1:8000/hello/ann) and
[http://127.0.0.1:8000/docs](http://127.0.0.1:8000/docs) (Swagger UI).

PostgreSQL, MySQL, MongoDB, and Redis examples need the matching URL. Start
them from the repository `docker-compose.yml`:

```bash
docker compose up -d --wait
```

The compose header lists host ports and credentials. Details are in
[Testing](/siderite/guides/production/testing/).

## Next

- [First application](/siderite/start/first-app/) — walk the Hello World app
- [Core concepts](/siderite/start/concepts/) — `App`, prelude, `Db`, `Schema` vs `Model`
