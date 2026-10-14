---
title: Crate map
description: Workspace crates, what each owns, and dependency rules.
---

The `siderite` crate is the public facade. Application code depends on it
and `use siderite::prelude::*;`. The other crates exist so HTTP, validation,
and the ORM do not form a cycle.

```
user application
        │
        ▼
     siderite (facade + prelude)
        │
        ├── siderite-core        App, routing, extractors, ApiError
        │       ├── siderite-validation
        │       └── siderite-orm
        ├── siderite-openapi
        ├── siderite-orm
        ├── siderite-config
        └── siderite-cache
                └── siderite-core

siderite-backends ──► siderite-orm
siderite-migrations ──► siderite-orm
siderite-macros  ·generates code for·  core, orm, validation
siderite-cli ──► migrations, config
siderite-testkit ──► core
```

| Crate | Owns |
|---|---|
| `siderite` | Public facade, `prelude` |
| `siderite-core` | `App`, handler/extractor/response traits, DI, middleware, lifespan, WebSockets, forms, headers/cookies, background tasks, static files, RFC 7807 errors, security schemes |
| `siderite-validation` | `Validate`, structured `ValidationError`, rules, constrained newtypes, `Schema` + `SchemaRegistry` |
| `siderite-orm` | `Model`, `QuerySet`, relations, transactions, signals, database routing, `QueryPlan` IR, `Expr`, capabilities |
| `siderite-backends` | Dialect-aware SQL compiler and executors: SQLite, PostgreSQL, MySQL; MongoDB (supported QuerySet subset); Redis key/hash/set client |
| `siderite-macros` | Route attributes, `routes![]`, `#[derive(Model, Validate, Schema)]`, `#[model_hooks]`, `#[receiver]` |
| `siderite-openapi` | Typed OpenAPI 3.1 model, document builder, Swagger UI / ReDoc |
| `siderite-migrations` | Migration graph, operations, autodetector, schema editor (PostgreSQL, SQLite, MySQL), executor |
| `siderite-config` | Layered configuration, `Secret`, `init_tracing` |
| `siderite-cache` | `Cache` trait, in-memory LRU, Redis cache, `RouteCache` middleware |
| `siderite-sessions` | Cookie sessions, CSRF tokens, `MemoryStore` and cache-backed `CacheStore`; depends on core, not re-exported by the facade |
| `siderite-cli` | `AppCli` and the standalone `siderite` binary |
| `siderite-testkit` | `TestClient`, `TestDatabase` |
| `siderite-bench` | Criterion benchmarks (not published) |

## Dependency rules

1. **The ORM has no HTTP knowledge.** `siderite-orm` does not depend on
   `siderite-core`. The core crate implements `From<OrmError> for ApiError`.
2. **Schema metadata lives in validation.** `Schema` / `SchemaObject` sit
   in `siderite-validation` so that core (request bodies) and openapi
   (documents) share one definition without a cycle.
3. **Backends depend on the ORM, never the reverse.** The ORM defines the
   `Backend` trait and the capability model; each backend implements them.
4. **HTTP traits are siderite’s own.** `FromRequestParts`, `FromRequest`,
   `IntoResponse`, and `Handler` carry `describe` hooks.
5. **Axum and SQLx are internal.** Public types are siderite newtypes
   (`Json`, `Path`, `Query`, `State`, `MethodRouter`, …).

## See also

- [Architecture](/siderite/internals/architecture/)
- [Prelude](/siderite/reference/prelude/)
- [API rustdoc](/siderite/reference/rustdoc/)
