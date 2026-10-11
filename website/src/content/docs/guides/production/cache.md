---
title: Cache
description: Cache trait, in-memory LRU, Redis cache, and RouteCache middleware.
---

`siderite::cache` (crate `siderite-cache`) provides a small async cache API,
two backends, and a response-caching middleware.

## The `Cache` trait

| Method | Behaviour |
|---|---|
| `get(key)` | `Option<Vec<u8>>`; expired entries count as missing |
| `set(key, value, ttl)` | store bytes; `None` TTL keeps the entry until evicted |
| `set_if_absent(key, value)` | atomic non-expiring creation; optional for custom backends |
| `set_if_absent_with_ttl(key, value, ttl)` | atomic expiring creation (zero TTL rejected, sub-millisecond rounded up, TTL above `i64::MAX / 2` ms rejected); implemented by `MemoryCache` and `RedisCache` (one `SET NX PX`); optional, default is `CacheError::Unsupported` |
| `delete(key)` | `true` when an entry was removed |
| `increment(key, by)` | Redis `INCRBY` semantics: a missing key starts at `0`, a non-integer value is an error |
| `clear()` | remove every entry (Redis: only this cache’s key prefix) |

`CacheExt` is implemented for every `Cache` and adds JSON helpers:

```rust
use siderite::cache::{CacheExt, MemoryCache};
use std::time::Duration;

let cache = MemoryCache::new(1024);
let stats: Stats = cache
    .get_or_set("stats", Some(Duration::from_secs(60)), || async {
        Ok(compute_stats().await)
    })
    .await?;
```

`get_or_set` returns a cached value when it decodes as `T`. Otherwise it
runs the closure and stores the result. An error from the closure is
returned and nothing is stored. Concurrent misses may each run the
closure; there is no stampede lock.

## Backends

- **`MemoryCache::new(capacity)`**: in-process LRU with per-entry TTL,
  bounded to `capacity` entries. The least recently used entry is evicted
  first.
- **`RedisCache`** (feature `redis`): `RedisCache::connect(url)` or
  `RedisCache::new(store)` on `RedisStore`. It uses the store’s key prefix
  and native TTLs. Live tests run when `REDIS_URL` points at database 15.

## Route caching

`RouteCache` caches whole `GET` and `HEAD` responses. Caching is
**opt-in**: a route is cached only when its response or its placement says
so.

**Per response.** Put the layer on the app. It stores only responses marked
`Cache-Control: public`; `Cached::public(ttl, response)` sets
`public, max-age=<ttl>`:

```rust
use siderite::cache::{MemoryCache, RouteCache};
use siderite::Cached;
use std::time::Duration;

#[get("/items")]
async fn list_items() -> Cached<Json<Vec<Item>>> {
    Cached::public(Duration::from_secs(60), Json(load_items().await))
}

let app = App::new()
    .routes(routes![list_items, me])   // `me` is never stored
    .layer(RouteCache::new(MemoryCache::new(1024)).max_body_bytes(256 * 1024));
```

**Per route.** Put the layer on one route with `MethodRouter::layer` and set
`default_ttl`; that route is cached without a `public` marker:

```rust
let app = App::new().route(
    "/stats",
    get(stats).layer(RouteCache::new(cache.clone()).default_ttl(Duration::from_secs(30))),
);
```

Do not set `default_ttl` on an app-wide layer unless every `GET` route in
the app is safe to share between users.

- **Key:** method, scheme, `Host`, path and query, and the request’s `Accept`
  and `Accept-Encoding` values. Each part is length-prefixed, so virtual
  hosts sharing one cache never see each other's responses and different
  header values cannot collide.
- **Stored only when** the status is `200`, there is no `Set-Cookie`, and
  `Cache-Control` has none of `no-store`, `no-cache` or `private`. `Vary`
  may name only `Accept` or `Accept-Encoding`. The body length must be known
  and at most `max_body_bytes` (1 MiB by default). The response must also be
  `public`, unless `default_ttl` is set.
- **Lifetime:** `s-maxage`, else `max-age`, else `default_ttl`. A `public`
  response with none of these, or a lifetime of zero, is not stored.
- **Bypass:** requests carrying credentials are neither answered from nor
  stored in the cache: `Authorization`, `Proxy-Authorization`, `Cookie`,
  `X-API-Key`, any header whose name contains `auth`, `token`, `session`,
  `jwt`, `secret`, `api-key`, `apikey` or `access-key`, and any header added
  with `bypass_header`. This is a backstop; the opt-in above is what keeps
  private responses out of the cache.
- **Streaming bodies** (server-sent events, `StreamingResponse`, file
  streams) are never buffered; they pass through untouched.
- **Marking:** every `GET`/`HEAD` response through the layer gets
  `x-cache: hit` or `miss`. A cached `HEAD` keeps the `Content-Length` of the
  original response.
- **Backend errors fail open:** the request is served as a miss.

## See also

- [Security](/siderite/guides/http/security/)
- [Middleware and lifespan](/siderite/guides/http/middleware/)
- [Backends](/siderite/guides/data/backends/) — Redis client


## Freshness, invalidation and isolation

Request `no-cache`, `max-age` and `min-fresh` refresh from the origin;
`Pragma: no-cache` does too. Request `no-store` bypasses lookup and storage
without deleting existing entries. Range and conditional requests bypass
this supported caching subset. Origin `Date` is preserved; origin `Age`
and apparent age reduce remaining freshness, and hits add residence time.
Connection-nominated headers and fixed hop headers are removed. Responses
with trailers or unknown body length are not buffered.

Successful unsafe writes (2xx/3xx) rotate the target's shared generation
across GET/HEAD and header variants, including authenticated writes.
Failed writes preserve entries. Random generations prevent delayed fills
and evicted generation markers from reviving older representations.
`invalidate_target(&request)` rotates a related URI after an application
write; list/detail dependencies are application policy.

Each layer defaults to a unique namespace. Use `.namespace("app-v2")` only
for applications or nodes intentionally sharing compatible responses and
invalidation. Cache key scheme uses `.trusted_proxies(...)`, the same exact
peer policy as redirect and rate limiting; untrusted forwarded headers are
ignored. The proxy must replace headers. Malformed trusted context fails.

The cache requires atomic `Cache::set_if_absent_with_ttl`; MemoryCache and
RedisCache implement it. Custom backends without it bypass route storage;
see [Generation lifetime](#generation-lifetime).
Backend read/store errors serve the origin. Failed write invalidation
disables the affected layer, preserving the already completed write.
Database commits and cache invalidation are separate operations: other
nodes or restarted processes can retain stale entries during a partition.
Use bounded TTLs and a coordinated invalidation/outbox policy where that
consistency matters; this middleware does not offer distributed atomicity.

## Generation lifetime

Each target's invalidation marker (its generation) expires after
`.generation_ttl(duration)`, one hour by default. Stored representations
never outlive it: their TTL is `min(response lifetime, generation_ttl)`.
A write deletes the marker, and the next request draws a fresh random
generation, so older or still-running fills can never be served. Markers are
created with the cache's atomic `set_if_absent_with_ttl`. A custom `Cache`
that does not implement it (it returns `CacheError::Unsupported`) makes
the layer bypass storage and serve every request from the origin with
`x-cache: miss`; the layer logs one warning, remembers this and stops
probing the backend for the life of the layer (restart after switching to a
capable backend). A `generation_ttl` of zero disables route caching without
touching the backend.

Because representations are clamped to `generation_ttl`, a `max-age`,
`s-maxage` or `default_ttl` above it (one hour by default) is effectively
capped: raise `generation_ttl` for longer-lived entries. When upgrading
with a persistent Redis, markers written by earlier versions have no
expiry and are kept until a write to that target deletes them (or until
the namespace changes).

## Byte budgets

MemoryCache defaults to 64 MiB of retained key/value bytes, a 2 MiB value
limit and a 4 KiB key limit. `MemoryCache::with_limits(capacity, limits)`
uses `MemoryCacheLimits` to change them. Entry capacity also remains a hard
limit. Writes evict least recently used entries to fit; oversized writes
fail without replacing the prior entry. Replacement, deletion, expiry,
clear and increment account bytes. Overflowing TTLs fail explicitly.
The budget covers retained key/value payloads; allocator metadata is
bounded by entry count, and temporary read copies are outside this budget.

RouteCache separately defaults to 4 KiB keys, 8 KiB stored header bytes,
2 MiB encoded entries and 1 MiB bodies. `.byte_limits(key, headers, encoded)`
and `.max_body_bytes(bytes)` configure admission; oversized responses pass
through. The versioned representation uses base64 binary payloads, avoiding
JSON numeric byte arrays and an extra whole-body clone. Old encodings miss.
Redis deployments must configure server capacity and eviction separately.
