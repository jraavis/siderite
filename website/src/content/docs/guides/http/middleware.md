---
title: Middleware and lifespan
description: Tower layers, built-in middleware, ordering, startup and shutdown hooks.
---

## Layers

`App::layer(layer)` takes any `tower::Layer` over `middleware::BoxService`
(`BoxCloneSyncService<http::Request<Body>, http::Response<Body>, Infallible>`).
`middleware::from_fn(|req, next| async { .. })` builds one from a function.

**The first layer registered is the outermost.** It sees the request first
and the response last. `state`, `provide`, and lifespan resources are
always inside all middleware, regardless of call order. Root-app middleware
also wraps 404s and the docs endpoints; a mounted child’s middleware wraps
only its own routes. Rejections from built-ins are RFC 7807 problems.

## Built-ins

| Type | App method | Behaviour |
|---|---|---|
| `Cors` | `cors` | origins / methods / headers / credentials / max-age; `permissive()`. Credentials plus “any” mirrors the request instead of `*` |
| `Compression` | `compression` | gzip + brotli, toggleable |
| `CsrfOrigin` | `csrf_origin` | 403 problem for unsafe methods whose `Sec-Fetch-Site`/`Origin`/`Referer` is not same-origin or a configured origin; `exempt(prefix)`, `only_with_cookie()`. See [Security](/siderite/guides/http/security/) |
| `TrustedHosts` | `trusted_hosts` | exact / `*.sub` / `*`; 400 problem otherwise |
| `HttpsRedirect` | `https_redirect` | 308 to `https://host/path?query`. Honours `X-Forwarded-Proto` only if `trust_forwarded_proto(true)` (off by default) |
| `RequestIdLayer` | `request_id` | Propagates a valid `x-request-id` or generates UUID v4; `RequestId` extractor |
| `RequestLogging` | `request_logging` | `http.request` span with request_id, method, matched route, status, latency_ms |
| `Timeout` | `timeout` | 504 problem by default (`.status(..)` to change); covers response production, not body streaming |
| `ConcurrencyLimit` | `concurrency_limit` | Bounded queue, deadline and optional body permits |
| `BodyLimit` | `body_limit` | 413 problem via `Content-Length`; streamed bodies are capped while read |
| `RateLimit` | `rate_limit` | Token bucket per client IP, 429 + `Retry-After` |

Logging never records headers (`Authorization`, `Cookie`, API keys), query
strings, or bodies. Register `request_id` before `request_logging`. Request
spans are documented in [Observability](/siderite/guides/production/observability/).
HTTP GET/HEAD response caching is `RouteCache`; see
[Cache](/siderite/guides/production/cache/).

`App::layer` wraps the whole app. To wrap a single route, call `.layer(..)`
on its `MethodRouter` instead (`get(h).layer(l)`); the layer then runs after
routing and only for that path.

`RateLimit` is **process-local**: with N replicas the effective limit is N
times higher, and a restart resets it. The client key is the socket peer
(available under `App::run`). Enable `trust_forwarded_for` only behind a
trusted proxy. Without any address (in-process tests) all requests share a
bucket.

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
Use `is_ready()` in health probes. Shutdown clears readiness and closes
transport admission before resource teardown. Probe routing, load balancer
removal and application/database health remain deployment responsibilities.
Services built without running lifespan remain Starting. In-process
WebSocket use requires retaining and supervising lifespan ownership.

## See also

- [Observability](/siderite/guides/production/observability/)
- [Cache](/siderite/guides/production/cache/)
- [Testing](/siderite/guides/production/testing/)

### Proxy trust and bounded rate limiting

Forwarded headers are ignored by default. Configure the same
`TrustedProxies::new` policy with exact immediate peer IPs on
`RateLimit::trusted_proxies` and `HttpsRedirect::trusted_proxies`. Trusted
proxies must replace client-supplied headers. Malformed or duplicate trusted
headers are rejected; exact-peer IP mode accepts one IP, not an arbitrary
proxy chain. Blanket trust requires explicit compatibility configuration.

Rate-limiter state defaults to 10,000 normalized IP keys. `max_clients`
configures a hard bound with fresh counters; new identities are denied at
saturation and depleted existing buckets are retained. Expiry checks at
most 16 candidates and removes only fully replenished buckets. `try_new`
validates numeric settings, and `App::rate_limit` rejects invalid settings.

`HttpsRedirect` preserves IPv6 brackets and removes incoming HTTP ports.
Set `canonical_authority` for a deployment HTTPS host or custom port, and
`allowed_hosts` to validate incoming hosts before redirect/origin dispatch.


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
