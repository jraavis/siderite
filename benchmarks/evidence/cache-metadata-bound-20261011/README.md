# Route-cache metadata bound (R05) — 2026-10-11

The per-namespace admission budget (`RouteCache::max_new_targets`, default
10,000 new markers per `generation_ttl` window) is enforced and measured.
Design: `docs/CACHE.md`, "Metadata resource bound".

## Bound regression

`crates/siderite-cache/tests/metadata_bound.rs` counts live marker, counter,
epoch and representation keys on three paths: `MemoryCache`'s atomic
admission, the generic admission an external adapter gets from the provided
`Cache::admit_marker` default, and live Redis (`SCAN` over the test prefix,
window indexes read from the server `TIME`). When no window boundary was
crossed during a scenario the budget is asserted exactly; otherwise the
two-window bound. All pass:

| Scenario | Result |
|---|---|
| 400 distinct cacheable targets, budget 50 | exactly 50 markers, 50 representations, 1 counter (2x budget only if a boundary was crossed); at most 64 epoch keys; only those 50 hit |
| 300 distinct uncacheable targets, budget 10 | 0 markers, 0 counters; 10 cacheable targets then admitted (not starved) |
| 8 namespaces x 100 targets, budget 10 | 80 markers (= namespaces x budget), at most 2 counters per namespace |
| 200 concurrent distinct first requests, budget 20 | exactly 20 markers (no overshoot) |
| 1.5 s of distinct targets over 250 ms windows, budget 10, census after every request | peak live markers at most 20 (2 x budget) |
| Budget 0 | nothing stored, no backend call |

Other tests: `crates/siderite-cache/tests/admission.rs` (an epoch evicted
from an 8-entry cache cannot revive a stale fill; a write between marker
creation and the epoch check refuses the fill; a write refuses first fills
only in its stripe; an adapter with the expiring insert but no increment
bypasses safely and leaves no marker, and the reverse spends no budget;
backend errors during the epoch read or the admission are safe misses;
per-path backend call counts), atomic admission/rotation and `EVALSHA` use
on live Redis (`tests/redis.rs`), and TTLs on the counter (two windows),
marker, representation and epoch keys.

## Latency and memory (10,000 admitted, 10,000 bypassed, 10,000 hits)

Shared development host, one process, sequential requests through the
in-process test client, debug test profile (not isolated release-gate
numbers). Redis is the local docker instance, `maxmemory-policy noeviction`,
database 15.

| Backend | Admit (new target) | Bypass (budget spent) | Hit |
|---|---:|---:|---:|
| MemoryCache | p50 86 us, p99 328 us | p50 20 us, p99 78 us | p50 34 us, p99 164 us |
| Redis | p50 1.61 ms, p99 5.2 ms | p50 0.43 ms, p99 1.8 ms | p50 0.66 ms, p99 2.3 ms |

A bypassed request is now one backend read (the spent window is remembered
in memory), so it is cheaper than a hit. Memory for 10,000 admitted targets
(marker + representation each): MemoryCache retained bytes 0 -> 4,290,419
(`MemoryCache::stored_bytes`); Redis `used_memory` 1,509,144 -> 9,502,104
(about 800 bytes per target, including the 64 epoch keys). Keys at the end:
10,000 markers, 10,000 representations, 1 counter, 64 epochs. The 10,000
bypassed targets added nothing. Server eviction configuration was only read,
never changed.

## Instruction counts (cachegrind)

Release example binary, `MemoryCache`, current-thread runtime, difference of
6,000 and 1,000 iterations divided by 5,000. Each tree was built from its own
git worktree/copy with its own target directory: R04 is `HEAD~1` of the R05
commit, R05 is that commit, new is this change. The three trees were
measured in the same session. Instructions per request:

| Path | R04 base | R05 commit | New |
|---|---:|---:|---:|
| hit | 48,716 | 48,758 | 40,677 (-16.5%) |
| refresh_fill (`no-cache` GET) | 66,161 | 66,227 | 58,359 (-11.8%) |
| write_then_fill (POST + GET) | 90,283 | 103,221 | 86,309 (-4.4%) |
| distinct targets, first request | 64,671 | 73,502 | 62,309 (-3.7%) |
| over-budget request (budget 1, window spent) | n/a | 47,094 | 27,402 |

All paths are at or below R04. The hit path makes the same two backend
calls as before; its saving is a table-driven hex encoder for the generation
in the representation key (it replaced sixteen `write!` calls) and
precomputed key prefixes. The write is one backend call (epoch replace plus
marker delete), tokens come from a per-layer seed and counter instead of
`getrandom`, admission is one backend call, and the over-budget path makes a
single read because the spent window is remembered.

Reproduce: `REDIS_URL=redis://127.0.0.1:56379/15 cargo test -p siderite-cache
--all-features --test metadata_bound -- --include-ignored --nocapture
--test-threads=1`. `run.log` holds the output of this run.
