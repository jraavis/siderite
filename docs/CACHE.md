# Cache

`siderite::cache` (crate `siderite-cache`) provides a small async cache API,
two backends and a response-caching middleware.

## The `Cache` trait

| Method | Behaviour |
|---|---|
| `get(key)` | `Option<Vec<u8>>`; expired entries count as missing |
| `set(key, value, ttl)` | store bytes; `None` TTL keeps the entry until evicted |
| `set_if_absent(key, value)` | atomic non-expiring creation; optional for custom backends |
| `set_if_absent_with_ttl(key, value, ttl)` | atomic expiring creation; optional, default is `CacheError::Unsupported` |
| `delete(key)` | `true` when an entry was removed |
| `increment(key, by)` | Redis `INCRBY` semantics: a missing key starts at `0`, a non-integer value is an error |
| `clear()` | remove every entry (Redis: only this cache's key prefix) |

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

`get_or_set` returns a cached value when it decodes as `T`. Otherwise it runs
the closure and stores the result. An error from the closure is returned and
nothing is stored. Concurrent misses may each run the closure; there is no
stampede lock.

## Backends

- **`MemoryCache::new(capacity)`**: in-process LRU with per-entry TTL, bounded
  to `capacity` entries. The least recently used entry is evicted first.
- **`RedisCache`** (feature `redis`): `RedisCache::connect(url)` or
  `RedisCache::new(store)` on the Phase 5 `RedisStore`. It uses the store's key
  prefix and native TTLs. Live tests run when `REDIS_URL` points at database
  15 (see [TESTING.md](TESTING.md)).

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

## Generation contract

Generations are the only mechanism that makes invalidation, expiry and
eviction safe for route caching. The invariants below are normative for
`RouteCache` and for every `Cache` backend it runs on.

1. **Fresh random token.** A target's generation is a random UUID. Any
   marker created after expiry, eviction, deletion or rotation carries a
   newly drawn token, never a counter or a value derived from the old one.
2. **Delayed fills stay dead.** Representation keys embed the generation.
   A fill that completes after rotation, expiry or eviction stores under an
   old key that no later reader computes.
3. **Representation TTL.** A representation's TTL is clamped to
   `min(response ttl, generation_ttl)`. A representation may outlive its
   marker by at most `generation_ttl`, but it is unreachable: a marker
   recreated after expiry carries a fresh token (invariant 1), so no reader
   computes the old key. (The marker's remaining lifetime is not
   observable, and the marker value is only a 16-byte token.)
4. **Atomic creation only.** Markers are created with
   `set_if_absent_with_ttl`. Expiry and insertion are one atomic step;
   `get` followed by `set` is never an acceptable substitute.
5. **Finite lifetime.** Markers carry a TTL (`generation_ttl`, default one
   hour). A marker without a TTL is a leak under unique targets.
6. **Invalidation deletes the marker.** A successful unsafe write (and
   `invalidate_target`) deletes the target's marker instead of writing a
   new one. A missing marker yields a fresh random token (invariant 1), so
   deletion rotates the generation, and invalidation never creates a
   marker. It does refresh one per-namespace epoch key (see "Fills that
   start without a marker"), which is a constant, not a per-target, cost.
   Rotating only an already existing marker, with the same TTL policy, is
   equally valid; writing a non-expiring marker is not.

Invariants 1 to 6 are enforced by `RouteCache` (roadmap R04); the metadata
bound below is enforced by R05.
`RouteCache::generation_ttl(duration)` configures the marker lifetime
(`DEFAULT_GENERATION_TTL`, one hour). Expiry of the marker while a fill is
in flight is a safe miss: the fill lands under the old token's key, and the
next request creates a marker with a new random token. The hit path is
unchanged: one marker `get` and one representation `get`.

Operational notes:

- `max-age`, `s-maxage` and `default_ttl` above `generation_ttl` (default
  one hour) are effectively capped to it; raise `generation_ttl` for
  longer-lived entries.
- A `generation_ttl` of zero disables route caching: no backend calls, no
  storage, no invalidation (`x-cache: miss`).
- The first `Unsupported` from `set_if_absent_with_ttl` logs one warning and
  disables storage for the life of the layer; the flag never resets.
- Upgrading with a persistent Redis: markers written by earlier versions
  have no expiry. They are kept until a write to that target deletes them,
  or until the namespace changes.

`set_if_absent_with_ttl` semantics: a zero TTL is rejected with
`CacheError::InvalidTtl`; TTLs round up to whole milliseconds (a 1 ns TTL
lives 1 ms, 1.5 ms lives 2 ms) so entries never expire earlier than asked;
an expired entry counts as absent; a losing creator leaves the winner's
value and deadline untouched; exactly one concurrent creator wins. A TTL
above `i64::MAX / 2` milliseconds is rejected with `InvalidTtl` (never
clamped), so every backend agrees and Redis never sees a deadline that
overflows its clock. `MemoryCache` implements this under one lock
acquisition; `RedisCache` sends a single `SET key value NX PX ms` on the
prefixed key, so a dropped future leaves either no key or an expiring one.

### Metadata resource bound

Marker metadata is bounded across all processes sharing a backend. Each
namespace admits at most `max_new_targets` (default 10,000,
`RouteCache::max_new_targets`) new markers per `generation_ttl` window. The
budget is a counter per window: the counter key embeds the window index
`floor(unix_time / generation_ttl)`, so each window has its own key and
nothing is shared across windows. Admission calls the optional
`Cache::increment_with_expiry` primitive (`Unsupported` by default, like
`set_if_absent_with_ttl`). It increments the key, creating it at zero when
missing, and gives it an expiry only when it has none, in one atomic step.
Redis runs `INCRBY` plus `PEXPIRE` (when `PTTL` is negative) in one `EVAL`
script; `MemoryCache` does both under one lock. Separate `set_if_absent` and
`increment` calls are not acceptable: if the counter expires or is evicted
between them, `INCRBY` recreates it without a TTL, which is a permanent
bypass. Eviction of the counter resets it, so the bound holds only while
the counter is not evicted; deployments that need the bound must not evict
these keys (for example Redis `noeviction` for the namespace or a dedicated
database). The layer never changes a Redis server's eviction configuration.

Because a marker lives at most `generation_ttl`, markers created in the
last `generation_ttl` come from at most two windows, so a namespace holds
at most `2 x max_new_targets` markers plus at most two counters and one
epoch key. Nodes whose clocks differ can disagree on the window index; keep
clocks synchronised, and treat a few seconds of skew as loosening the bound
only at window edges. Each marker is a key of at most `byte_limits` key
bytes (default 4 KiB) plus a 16-byte value plus per-key backend overhead
(about 50 to 90 bytes in Redis), so the default worst case is approximately
85 MB per namespace and typically far less. Measured: 10,000 admitted
targets with short keys cost about 4.3 MB in `MemoryCache` and 8.0 MB of
Redis `used_memory`, including their representations (see
`benchmarks/evidence/cache-metadata-bound-20261011/`). Targets whose key
exceeds the key limit never create a marker.

A marker is counted against the budget only when a representation is
actually stored under it, not when the marker is first looked up. Otherwise
about 10,000 unique `GET`s to uncacheable responders would exhaust the
budget and starve cacheable targets. Markers are therefore created lazily:
a request that finds no marker proceeds to the origin, and only a response
that will be stored creates the marker (after a re-read, so concurrent
creators share one and are counted once) and consumes one budget unit. The
counter can exceed the number of live markers (a creator that lost a race
or whose fill was refused still counted); this only makes the bound
conservative.

When the window budget is exhausted, or the target is uncacheable
(credentials, `no-store` request, non-`GET`/`HEAD`, oversized key),
the layer **bypasses**: it serves the origin response, creates no marker,
stores no representation and marks the response `x-cache: miss`. Targets
that already have a marker keep hitting and refilling for the rest of the
window. Unique targets (for example per-request query strings) therefore
cannot grow metadata beyond the bound. `max_new_targets(0)` stores nothing
and makes no backend calls.

The hit path is unchanged: a request whose marker exists performs the same
single marker `get` and single representation `get`, with no budget
accounting. Budget and epoch work run only for requests that found no
marker (first request per target per window) and, for admission, only when
a response is storable.

If the backend returns `Unsupported` from `increment_with_expiry`, the bound
cannot be enforced, so the layer fails closed exactly as it does for
`set_if_absent_with_ttl`: it logs one warning, stops storing for the life
of the layer and serves every response as `x-cache: miss`. It never falls
back to unbounded markers. An adapter that wraps another `Cache` must
forward `increment_with_expiry` (`Arc<T>` and `MemoryCache`/`RedisCache`
already do).

#### Fills that start without a marker

This is a correction to the original design. With eager marker creation a
write that completes while a fill is in the origin deletes the marker the
fill is using, so the stale fill lands under a dead key (invariant 2).
With lazy creation a request that found no marker has nothing for the
write to delete, so a naive late fill would create a new marker and store
pre-write data under it. The design therefore adds a per-namespace
**epoch** key: every successful write or `invalidate_target` first replaces
it with a fresh random token (TTL `generation_ttl`) and then deletes the
target marker. A request that finds no marker snapshots the epoch before
the origin runs. At admission the fill creates or finds the marker first,
then re-reads the epoch and stores only if it is unchanged, so any write
either changed the epoch before the check (fill refused) or deletes the
already created marker after it (fill unreachable). A request that outlives
one `generation_ttl` is also refused, so an expired epoch cannot repeat a
token. Cost: one extra `get` before the origin when no marker exists, two
`get`s plus the increment and marker creation at admission, and one extra
`set` per write. The epoch is one key per namespace.

### Retired namespaces

Namespaces are never scanned or deleted. When a namespace is retired
(layer dropped, process restarted with the default unique namespace, or an
`app-vN` name abandoned) its markers and representations simply expire
within `generation_ttl`. Total metadata is therefore at most
`live_namespaces x 2 x max_new_targets` markers, plus a tail of retired
namespaces for up to one `generation_ttl`. Use a fixed `.namespace(...)` in
long-lived deployments so restarts do not orphan namespaces.

### External `Cache` adapters

`set_if_absent_with_ttl` and `increment_with_expiry` are provided methods
whose defaults return `CacheError::Unsupported`, so existing adapters keep
compiling unchanged; route caching stays off (one warning) until both are
implemented atomically.
`RouteCache` treats that error as a backend limitation: the first
`Unsupported` clears a shared capability flag, and from then on every
request is served from the origin with `x-cache: miss`, without any further
backend round trip (no marker `get`, no creation attempt, no invalidation
delete). It never falls back to non-expiring markers or to non-atomic
emulation. Adapters opt in by implementing the method atomically (for
Redis: a single `SET key value NX PX ms`). `Arc<T>` forwards it.

Status: invariants 1 to 6 and the metadata bound are enforced (R04, R05)
on `MemoryCache` and live Redis. Load regressions drive sustained distinct
targets, uncacheable responses, namespace churn, concurrent bursts and
window rollover and count live marker, counter, epoch and representation
keys against the bound (`crates/siderite-cache/tests/metadata_bound.rs`;
the Redis variants are `#[ignore]` and need `REDIS_URL`).

### Failures and consistency

Backend read/store errors serve the origin. Failed write invalidation
disables the affected layer, preserving the already completed write.
Database commits and cache invalidation are separate operations: other
nodes or restarted processes can retain stale entries during a partition.
Use bounded TTLs and a coordinated invalidation/outbox policy where that
consistency matters; this middleware does not offer distributed atomicity.

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
