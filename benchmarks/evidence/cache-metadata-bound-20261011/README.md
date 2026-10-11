# Route-cache metadata bound (R05) — 2026-10-11

The per-namespace admission budget (`RouteCache::max_new_targets`, default
10,000 new markers per `generation_ttl` window) is enforced and measured.
Design: `docs/CACHE.md`, "Metadata resource bound".

## Bound regression

`crates/siderite-cache/tests/metadata_bound.rs` counts live marker, counter,
epoch and representation keys (MemoryCache through a key-recording adapter;
Redis with `SCAN` over the test prefix). Both backends pass:

| Scenario | Result |
|---|---|
| 400 distinct cacheable targets, budget 50 | 50 markers, 50 representations, 1 counter, only those 50 hit |
| 300 distinct uncacheable targets, budget 10 | 0 markers, 0 counters; 10 cacheable targets then admitted (not starved) |
| 8 namespaces x 100 targets, budget 10 | 80 markers (= namespaces x budget), at most 2 counters per namespace |
| 200 concurrent distinct first requests, budget 20 | 20 markers (limit 2 x budget) |
| 1.5 s of distinct targets over 250 ms windows, budget 10 | peak live markers at most 20 (2 x budget) |
| Budget 0 | nothing stored, no backend call |

Other tests: atomic increment-with-expiry under 32 concurrent callers on
live Redis, TTLs on the budget counter, marker, representation and epoch keys,
and the existing write-during-fill test (a fill that began with no marker
cannot store after a write).

## Latency and memory (10,000 admitted, 10,000 bypassed, 10,000 hits)

Shared development host, one process, sequential requests through the
in-process test client (not isolated release-gate numbers). Redis is the
local docker instance, `maxmemory-policy noeviction`, database 15.

| Backend | Admit (new target) | Bypass (budget spent) | Hit |
|---|---:|---:|---:|
| MemoryCache | p50 83 us, p99 274 us | p50 45 us, p99 183 us | p50 35 us, p99 139 us |
| Redis | p50 1.81 ms, p99 5.6 ms | p50 1.21 ms, p99 4.4 ms | p50 0.72 ms, p99 3.2 ms |

Memory for 10,000 admitted targets (marker + representation each):
MemoryCache retained bytes 0 -> 4,287,602; Redis `used_memory`
1,328,760 -> 9,340,624 (about 800 bytes per target). Keys at the end: 10,000
markers, 10,000 representations, 1 counter. The 10,000 bypassed targets added
nothing. Server eviction configuration was only read, never changed.

## Hit-path instruction counts (cachegrind)

Release binary, `MemoryCache`, current-thread runtime, difference of 6,000
and 1,000 iterations divided by 5,000. Base is HEAD (R04) built in a
separate worktree and target directory.

| Path | Base instr/request | New instr/request |
|---|---:|---:|
| hit | 48,661 | 48,683 (+22, +0.05%) |
| refresh_fill | 66,309 | 66,485 (+0.3%) |
| write_then_fill | 90,417 | 103,895 (+15%) |
| distinct targets, first request | 64,478 | 72,806 (+13%) |

The hit path gains no cache round trip, allocation or lock; the +22
instructions are code layout. Budget, epoch and admission work run only on
the store path (an extra epoch `get`, the counter increment and a marker
re-read) and on writes (one epoch `set`), which is why `write_then_fill` and
first requests cost more.

Reproduce: `REDIS_URL=redis://127.0.0.1:56379/15 cargo test -p siderite-cache
--all-features --test metadata_bound -- --include-ignored --nocapture
--test-threads=1`. `run.log` holds the output of this run.
