---
title: Observability
description: http.request and orm.query tracing spans, redaction rules, and JSON logs.
---

siderite emits [`tracing`](https://docs.rs/tracing) spans and events.
Install a subscriber in your binary (`siderite::config::init_tracing` builds
one from `LogSettings`; `RUST_LOG` wins) and the spans below appear in
your logs or exporter.

Bind parameters, passwords, tokens, API keys, and `Secret` values are
never recorded.

## HTTP request spans

`RequestLogging` wraps each request in an `info`-level `http.request` span:

| Field | Value |
|---|---|
| `request_id` | the id set by `RequestIdLayer` (register it before `RequestLogging`) |
| `method` | HTTP method |
| `route` | the matched path template (`/users/{id}`), or `<unmatched>`; never the raw URI |
| `status` | response status code, recorded when the response is ready |
| `latency_ms` | time spent in the inner service |

```rust
let app = App::new()
    .routes(routes![get_user])
    .request_id()
    .request_logging();
```

Headers (including `Authorization`, `Cookie`, and API keys), query strings,
and bodies are never recorded. Handler spans and ORM `orm.query` spans nest
inside `http.request`, so one request id ties a request to its queries.

Install a subscriber with `siderite::config::init_tracing(&settings.log)`.
`RUST_LOG` overrides `log.level`, and `log.json = true` switches to JSON
output.

## ORM query spans

Every `Db` entry point that talks to the database runs inside an
`orm.query` span at `debug` level:

| Field | Value |
|---|---|
| `db.system` | backend name (`sqlite`, `postgresql`, `mysql`, `mongodb`) |
| `db.operation` | `select`, `insert`, `update`, `delete`, `raw`, or `script` |
| `db.table` | the table the plan targets; empty for raw SQL and scripts |
| `elapsed_ms` | duration in milliseconds, recorded when the call finishes |

Enable it with `RUST_LOG=siderite_orm=debug`.

SQL text is logged only by the raw entry points and script execution, and
only at `trace` level (`RUST_LOG=siderite_orm=trace`). Because raw SQL is
written by the developer, keep secrets out of it and pass values as bind
parameters. Parameters are never recorded, at any level.

## Liveness

```rust
use siderite::health::liveness;

let app = App::new().routes([liveness("/livez")]);
```

Liveness is opt-in: `App::routes([siderite::health::liveness("/livez")])`
registers `GET /livez` answering `200` with `{"status":"alive"}` and
`Cache-Control: no-store` in every phase, reading no database, readiness,
version or host details (documented under the `health` tag, operation ID
`liveness`; `liveness_router()` mounts it with `App::route`). Nothing is
registered unless asked, and a path the app already uses is rejected when
the app is built. The server binds only after startup and stops accepting
connections when shutdown starts, so probes get no connection before
startup completes or once draining begins; size the probe's failure
threshold to cover the shutdown timeout.

```rust
use siderite::health::{ReadinessChecks, readiness, sql_check};

let checks = ReadinessChecks::new().check("database", sql_check(db.clone()));
let app = App::new().routes([readiness("/readyz", checks)]);
```

Readiness with checks is opt-in too:
`readiness("/readyz", ReadinessChecks::new().check("database", sql_check(db)))`
answers `200` only while the server is Ready and every check passes, `503`
otherwise (no check runs before startup completes or once draining
begins). Checks are closures returning `Result<(), String>` that reuse the
app's existing pools and clients; `sql_check(db)` runs `SELECT 1` through a
`Db` (SQL backends only; write a check with your own client for Redis or
MongoDB). Probe traffic cannot become load: each check has a timeout
(`.timeout`, default 2 s), at most `.concurrency` (default 4) run at once,
and one evaluation is cached for `.cache` (default 1 s) and shared, so
concurrent probes wait for the evaluation in progress instead of starting
their own. The body is `{"status":"ready"|"unavailable","checks":{"name":
"ok"|"failed"|"timeout"}}` with `Cache-Control: no-store`; error details
are logged at `warn`, never returned, so name checks generically.

## See also

- [Configuration](/siderite/guides/production/config/)
- [Middleware and lifespan](/siderite/guides/http/middleware/)
- [Errors](/siderite/guides/http/errors/)
