# Middleware and lifespan

## Layers

`App::layer(layer)` takes any `tower::Layer` over `middleware::BoxService`
(`BoxCloneSyncService<http::Request<Body>, http::Response<Body>, Infallible>`).
`middleware::from_fn(|req, next| async { .. })` builds one from a function.

**Ordering: the first layer registered is the outermost.** It sees the request
first and the response last. `state`, `provide` and lifespan resources are
always inside all middleware, regardless of call order. Root-app middleware
also wraps 404s and the docs endpoints; a mounted child's middleware wraps
only its own routes. Rejections from built-ins are RFC 7807 problems.

## Built-ins

| Type | App method | Behaviour |
| --- | --- | --- |
| `Cors` | `cors` | origins/methods/headers/credentials/max-age; `permissive()`; credentials + "any" mirrors the request instead of `*` |
| `Compression` | `compression` | gzip + brotli, toggleable |
| `CsrfOrigin` | `csrf_origin` | 403 problem for unsafe methods whose `Sec-Fetch-Site`/`Origin`/`Referer` is not same-origin or a configured origin; `exempt(prefix)`, `only_with_cookie()`; see [SECURITY.md](SECURITY.md) |
| `TrustedHosts` | `trusted_hosts` | exact / `*.sub` / `*`; 400 problem otherwise |
| `HttpsRedirect` | `https_redirect` | 308 HTTPS redirect; forwarded scheme requires explicit proxy trust |
| `RequestIdLayer` | `request_id` | propagates a valid `x-request-id` or generates UUID v4; `RequestId` extractor |
| `RequestLogging` | `request_logging` | `http.request` span with request_id, method, matched route, status, latency_ms |
| `Timeout` | `timeout` | 504 problem by default (`.status(..)` to change); covers response production, not body streaming |
| `ConcurrencyLimit` | `concurrency_limit` | bounded queue, deadline and optional body permits |
| `BodyLimit` | `body_limit` | 413 problem via `Content-Length`; streamed bodies are capped while read |
| `RateLimit` | `rate_limit` | token bucket per client IP, 429 + `Retry-After` |

Logging never records headers (Authorization, Cookie, API keys), query strings
or bodies; a test asserts this. Register `request_id` before `request_logging`.
Request spans are documented in [OBSERVABILITY.md](OBSERVABILITY.md). HTTP GET/HEAD
response caching is `RouteCache` in `siderite-cache`; see [CACHE.md](CACHE.md).

`RateLimit` is **process-local**: with N replicas the effective limit is N times
higher and a restart resets it. The client key is the socket peer (available
under `App::run`). Configure `TrustedProxies::new` with exact immediate
proxy IP addresses; these peers must replace forwarded headers rather than
append client-provided values. Both `RateLimit::trusted_proxies` and
`HttpsRedirect::trusted_proxies` use this policy. Untrusted or unknown peers
cannot override client identity or scheme. Trusted malformed/duplicate
headers are rejected. Exact-peer client headers contain one IP; the explicit
blanket compatibility methods retain first-hop behavior for forwarded IP.
Without any address (in-process tests) all requests share a bucket.

Rate-limiter state defaults to at most 10,000 typed client keys. Configure
`max_clients` for a different positive bound; configuring capacity creates
fresh counters without changing existing clones. At saturation new identities
receive 429 while depleted existing clients retain their limits. Expiry scans
at most 16 candidates per admission and removes only fully replenished
buckets. IP normalization includes compressed IPv6 and mapped IPv4 forms.
`try_new` rejects zero burst, nonpositive refill and nonfinite values;
`App::rate_limit` also rejects invalid configuration at build time.

HTTPS redirect ignores forwarded scheme by default. For a custom HTTPS host
or port, pass a typed authority to `canonical_authority`; otherwise incoming
HTTP ports are removed and IPv6 brackets are preserved. `allowed_hosts`
validates incoming authority before a redirect or origin response. Duplicate
hosts, user info and invalid ports are rejected. Blanket forwarded-header
trust remains available only through explicit compatibility configuration.

Process-local background tasks have separate bounded admission and lifespan
ownership; see [background tasks](BACKGROUND_TASKS.md).

## Lifespan

- `on_startup` / `on_shutdown`: startup follows registration order;
  shutdown reverses it, including mounted apps. Startup failure and bind
  failure unwind initialized resources and preserve the original error.
  Hook panics become lifespan errors. All cleanup hooks are attempted;
  the first cleanup error is returned when no earlier error takes priority.
- `lifespan_resource(|| async { Ok((value, teardown_future)) })` exposes
  `Resource<T>` after initialization. Its teardown runs only after successful
  initialization. Requests get 503 before startup and after teardown.
  An initializer owns cleanup of partial work before returning its resource.
- `App::run` accepts Ctrl-C and Unix SIGTERM. `run_until(addr, future)`
  supports an explicit termination request, including during startup.
  `shutdown_timeout(Duration)` configures a positive budget, defaulting to
  30 seconds. HTTP draining and resource teardown share that deadline;
  exceeding it returns `ServerError::ShutdownTimeout`.
- `Lifespan::supervise()` returns `ManagedLifespan`. Await `ready()`, then
  consume it with `shutdown()`. Its supervisor owns cleanup if the caller
  cancels startup/shutdown or drops the owner. The Tokio runtime must remain
  alive; process crashes and runtime destruction cannot guarantee cleanup.
  Direct `Lifespan::startup`/`shutdown` callers own cancellation handling.
- `TestClient::start(app)` uses supervision. Explicit shutdown waits for
  cleanup; dropping the last client requests it. `TestClient::new` runs no
  hooks. Dropping a shutdown future leaves the cleanup supervisor running.

Accepted sockets, HTTP/2 protocol workers and pending/established WebSocket
callbacks are owned and cancelled before resource teardown on deadline
expiry. Cancelling the serving future also aborts connection work; the
lifespan supervisor waits for those drops before cleanup. Futures must
cooperate with polling; blocking code and process/runtime destruction cannot
guarantee asynchronous cleanup.

`App::server_limits(ServerLimits { ... })` bounds accepted sockets (default
1024), concurrent HTTP/2 streams per connection (100) and active/pending
WebSocket upgrades across the app tree (128). Upgrade overload returns 503
with Retry-After. Socket saturation leaves peers in the OS listener backlog;
closing an idle peer releases capacity. Invalid bounds fail configuration.

The `Readiness` extractor exposes Starting, Ready, Draining and Stopped.
Use `is_ready()` in health probes; `stopping().await` resolves once
shutdown begins, so long-lived responses (event streams, for example with
`Sse::until`) can end themselves before the shutdown timeout. Shutdown clears readiness and closes
transport admission before resource teardown. Probe routing, load balancer
removal and application/database health remain deployment responsibilities.
Services built without running lifespan remain Starting. In-process
WebSocket use requires retaining and supervising lifespan ownership.

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


## Bounded concurrency and streaming scope

`ConcurrencyLimit::new(max)` caps execution, allows at most `max` queued
requests and gives admission a five-second deadline. Queue saturation,
closed admission and deadline expiry return 503 with `Retry-After: 1`.
Use `.queue(max_waiting, timeout)` to change the queue; zero slots reject
immediately when execution is full. `.try_new(max)` validates a positive
capacity. Oversized capacities or invalid deadlines fail configuration;
`new(0)` retains its historical minimum of one execution slot.

Execution normally ends when the handler produces a response.
`.hold_body(true)` holds the same execution permit until the response body
completes, fails or is dropped, without buffering it. This bounds slow
streams but can reduce handler throughput. Empty bodies release promptly.
HTTP upgrades need separate WebSocket/task ownership; body limits do not
bound upgraded connection lifetime or the server's accepted TCP sockets.

Clones share admission and counters. Configure `.queue(...)` before cloning;
it creates fresh admission state. `.close()` rejects new requests and wakes
waiters while existing execution can complete. Use it during readiness
transitions when retaining a layer handle. Lifespan resource shutdown and
server connection drain remain separate policies.

`.stats()` exposes approximate active/waiting counts, admissions,
rejections, queue timeouts and cumulative completed queue-wait nanoseconds.
Aborted waits release capacity but do not add completed-wait timing.
Middleware registration is outermost first: register `Timeout` before the
limit to cover queue waiting and execution, or after it to time execution
only. Timeout does not extend to body streaming; configure stream deadlines
at the application/transport layer.
