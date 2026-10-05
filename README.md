# siderite

A FastAPI-style Rust web framework with Pydantic-style validation and a Django-style ORM. It is async-first, type-safe, and targets stable Rust (edition 2024, MSRV 1.99).

**Documentation:** [jraavis.github.io/siderite](https://jraavis.github.io/siderite/)

> **Status: Phase 6 (production tooling) complete, pre-alpha.** The APIs will change. The [architecture](https://jraavis.github.io/siderite/internals/architecture/) page lists what is implemented.

```rust
use siderite::prelude::*;

#[derive(Deserialize, Validate, Schema)]
struct Greeting { shout: Option<bool> }

#[derive(Serialize, Deserialize, Validate, Schema)]
struct Message {
    #[field(min_length = 1, max_length = 280)]
    message: String,
}

/// Greet somebody by name.
#[get("/hello/{name}", tag = "greetings")]
async fn hello(Path(name): Path<String>, Query(q): Query<Greeting>) -> PlainText<String> {
    let msg = format!("hello, {name}");
    PlainText(if q.shout.unwrap_or(false) { msg.to_uppercase() } else { msg })
}

#[post("/echo", status = 201)]
async fn echo(Json(m): Json<Message>) -> Json<Message> { Json(m) }

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    App::new().title("Hello").routes(routes![hello, echo]).run("127.0.0.1:8000").await
}
```

Invalid input is rejected with a `422` that lists every error with its location. OpenAPI 3.1 is generated from the handler signatures and served at `/openapi.json`, `/docs` (Swagger UI) and `/redoc`.

Models are structs with `#[derive(Model)]`. Queries are lazy `QuerySet`s built from typed field constants:

```rust
let adults = User::objects(&db)
    .filter(User::age.ge(18).and(User::name.icontains("ann")))
    .order_by([User::created_at.desc()])
    .limit(20)
    .all()
    .await?;
```

## Workspace

| Crate | Purpose |
|---|---|
| `siderite` | Facade and prelude. Most users depend only on this crate. |
| `siderite-core` | App, routing, extractors, responses, RFC 7807 errors |
| `siderite-validation` | Validation errors, rules, constrained types, schema metadata |
| `siderite-orm` | `Model`, `QuerySet`, relations, transactions, QueryPlan IR |
| `siderite-backends` | SQL compiler and executors: SQLite (default), PostgreSQL, MySQL, MongoDB (subset); Redis key/hash/set client |
| `siderite-testkit` | In-process `TestClient`, `TestDatabase` fixtures and isolation |
| `siderite-config` | Layered configuration (TOML, environment, overrides), `Secret` |
| `siderite-cache` | Cache trait, in-memory LRU and Redis caches, `RouteCache` middleware |
| `siderite-macros` | Route attributes, `routes![]`, `#[derive(Model, Validate, Schema)]` |
| `siderite-openapi` | OpenAPI 3.1 model, builder, docs UIs |
| `siderite-migrations` | Autodetector, JSON migrations, schema editor |
| `siderite-cli` | `siderite` binary (`new`, `run`, migrations) and `AppCli` |
| `siderite-bench` | Criterion benchmarks (not published) |

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo install --path crates/siderite-cli
cd examples/hello_world && siderite run
```

Examples (in `examples/`):

| Example | Shows |
|---|---|
| `hello_world` | Minimal app: routes, path/query extractors, JSON validation |
| `todo_sqlite` | ORM models and CRUD on SQLite |
| `blog_postgres` | PostgreSQL blog API driven by `AppCli`, with signals (needs `DATABASE_URL`) |
| `todo_mongo` | Todo API on MongoDB (needs `MONGODB_URL`) |
| `polyglot` | Users and analytics on two databases via database routing (`USERS_DATABASE_URL`, `ANALYTICS_DATABASE_URL`) |

Guides: [configuration](https://jraavis.github.io/siderite/guides/production/config/) and [cache](https://jraavis.github.io/siderite/guides/production/cache/).

Backends other than SQLite are behind cargo features (`postgres`, `mysql`, `mongodb`, `redis`). Their live tests are skipped unless a server URL is set:

| Backend | Feature | Live-test variable |
|---|---|---|
| PostgreSQL | `postgres` | `DATABASE_URL=postgres://...` |
| MySQL 8 | `mysql` | `MYSQL_URL=mysql://...` |
| MongoDB | `mongodb` | `MONGODB_URL=mongodb://...` |
| Redis | `redis` | `REDIS_URL=redis://...` |

To run them locally, start the databases from `docker-compose.yml` and export the URLs listed in its header ([testing](https://jraavis.github.io/siderite/guides/production/testing/)):

```bash
docker compose up -d --wait
cargo test --workspace --all-features
```

Unsupported features fail with a `BackendCapabilityError` before any I/O; see the [backend matrix](https://jraavis.github.io/siderite/reference/backend-matrix/). Redis is a key/hash/set client, not a `QuerySet` backend.

## Benchmarks

In-process Criterion benches (`cargo bench -p siderite-bench`) compare routing, extraction and the ORM against raw axum/serde; medians are in [`crates/siderite-bench/README.md`](crates/siderite-bench/README.md).

End-to-end throughput versus FastAPI (ApacheBench, same machine, median of 3 runs, 0 failures; automated suite in [`benchmarks/`](benchmarks/), see [benchmarks](https://jraavis.github.io/siderite/contributing/benchmarks/) for methodology):

| Test | Siderite | FastAPI |
|---|---|---|
| `GET /` (`hello_world`) | 36,281 req/s | 4,321 req/s |
| `POST /echo` (JSON) | 26,265 req/s | 3,817 req/s |
| Todo list 20 (SQLite) | 26,659 req/s | 5,249 req/s |
| Todo list 20 (PostgreSQL) | 9,044 req/s | 7,873 req/s |
| Todo list 20 (MySQL) | 9,396 req/s | 5,147 req/s |
| Todo list 20 (MongoDB) | 11,214 req/s | 3,717 req/s |

DB writes are DB-bound and land near parity; reads favor siderite. Numbers are machine-specific snapshots, not guarantees.

## Security defaults

- Request bodies read with `Body::into_bytes` are capped at 2 MiB (`413` above it), like the `Json` and `Form` extractors. Use `into_bytes_limited` for another cap.
- Configuration errors redact values of secret-looking keys (`*_url`, `*token*`, `*secret*`, `*passw*`, `*_key`, ...).
- `RouteCache` is opt-in: it stores only responses marked `Cache-Control: public` (see `Cached::public`), or every response of a single route whose layer sets `default_ttl`. It keys on scheme, host, path, query, `Accept` and `Accept-Encoding`, and skips requests with credential-like headers (`Authorization`, `Cookie`, `X-API-Key`, names containing `auth`, `token`, `session`, `jwt`, `secret`, `api-key` or `access-key`). See the [cache guide](https://jraavis.github.io/siderite/guides/production/cache/).

## Roadmap

The initial framework milestones are implemented: HTTP routing and OpenAPI,
validation and serialization, ORM and migrations, database adapters, and
production tooling. MongoDB supports a QuerySet subset; Redis is a typed
client. Reliability and compatibility work continues beyond these milestones.

The next goal is **daily application development through `siderite` commands,
without invoking Cargo directly**. Rust and Cargo remain the underlying build
toolchain; setup, development, validation and packaging get one CLI workflow.

Planned work, not yet implemented:

- [ ] **Reliability:** bounded Redis cache-generation metadata, remaining
  migration crash/disconnect tests, and enforced module-size limits.
- [ ] **Complete CLI workflow:** setup/doctor, watch and restart with `dev`,
  dependency management, route/model/CRUD generation, formatting, linting,
  unified `verify`, and OpenAPI export.
- [ ] **AI-assisted development:** version-matched local documentation,
  structured diagnostics, bounded task-focused context, safe instruction-file
  integration, validation reports, and an optional read-only MCP server.
- [ ] **Application productivity:** verified templates, local service fixtures,
  seed datasets, generated tests, and effective configuration inspection.
- [ ] **Framework additions:** HTTP pagination integration, health/readiness,
  SSE, and scoped SPA fallback. Cursor pagination and session/CSRF support
  begin with explicit design contracts.
- [ ] **Delivery and compatibility:** OpenAPI comparison, CI templates,
  reproducible release packaging, and framework upgrade previews.

The detailed plans contain **66 small tasks**, with dependencies, acceptance
criteria and release gates:

- [Core roadmap and 46 tasks](docs/plans/CLI_AI_ROADMAP.md)
- [Developer workflow: 20 tasks](docs/plans/DEVELOPER_EXPERIENCE.md)

First deliver structured checks/OpenAPI export, reliable development reload,
unified verification and version-correct AI context. Keep reliability fixes
independently releasable. The final workflow acceptance test must create,
change, test and package an application using only `siderite` commands.

Custom SDK generation, Studio, storage adapters, durable queues and
multi-tenancy remain future design scope, not current release commitments.

## License

Licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
