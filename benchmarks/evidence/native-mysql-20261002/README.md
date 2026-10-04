# Native MySQL experiment: 2026-10-02

## Result and decision

The retained-session mysql_async pool improved paired throughput by 20.4%
against SQLx in this local driver workload. Median p99 also decreased.
With one held connection per worker, the paired ratio was 1.009x and its
interval crossed parity. This supports pool recycling as the main source of
the observed advantage; it does not establish a large execution-engine gain.

Proceed with an opt-in native MySQL adapter that owns session and protocol
state explicitly. Do not swap drivers with default settings: native release
reset plus required session restoration was slower than SQLx here.
Do not make the native adapter the default before the complete ORM, failure
and migration contracts and end-to-end comparison pass.

## Measurements

Five paired passes per mode; ten workers and ten connections. Every insert
was a separate acknowledged autocommit, with the same prepared SQL and
bound values. Each pooled trial inserted 50,000 rows; each held trial
inserted 60,000. Within each profile every driver used the same count.
All 30 timed trials exceeded ten seconds. Warm-up/reset was outside timing.

| Profile | Driver mode | Median commits/s | Median p99 ms |
| --- | --- | ---: | ---: |
| Pooled | SQLx | 4,008 | 6.627 |
| Pooled | Native reset + setup | 2,704 | 6.750 |
| Pooled | Native retain | 4,656 | 5.409 |
| Held | SQLx | 4,785 | 4.843 |
| Held | Native reset + setup | 4,832 | 4.679 |
| Held | Native retain | 4,861 | 4.737 |

Paired ratios use the geometric mean of per-pass native/SQLx throughput,
which differs from dividing the displayed medians:

| Profile | Native mode | Paired ratio | Bootstrap 95% interval |
| --- | --- | ---: | --- |
| Pooled | Reset + setup | 0.681x | 0.589x-0.749x |
| Pooled | Retain | 1.204x | 1.140x-1.280x |
| Held | Reset + setup | 0.938x | 0.837x-1.019x |
| Held | Retain | 1.009x | 0.965x-1.056x |

Method: 20,000 bootstrap resamples of the five paired log ratios, random
seed 20261002, exponentiated means, sorted sample endpoints 500 and 19,500
(one-based). Five pairs are limited evidence; the interval is descriptive
for this local experiment, not a guarantee for another workload/deployment.

## Validation and environment

- MySQL 8.4.11, local Docker via loopback TCP, SQLx 0.8.6, mysql_async 0.37.1.
- `innodb_flush_log_at_trx_commit=1`, `sync_binlog=1`, binlog enabled.
  These record configured durability, not a physical power-loss test of
  Docker's host storage. Storage/CPU/environment metadata accompanies runs.
- UTC, autocommit and SQL mode setup aligned; every connection warmed.
  The native reset profile restores session setup after each reset.
- Every one of 1,650,000 measured acknowledgements was checked against the
  complete stored ID set and payload using an independent SQLx connection.
  Both disposable databases were dropped after successful completion.
- Live one-slot checks passed for typed rollback-on-drop, query cancellation,
  checkout cancellation and session reset/retention. Uneven worker allocation
  and queueing also passed for the complete three-mode measurement path.
- Workspace format, strict all-target/all-feature lint and all-feature tests
  passed; ignored live tests were excluded from that workspace run. The
  native live contracts ran separately. [Validation record](validation.json).
- Probe strict lint, Rust 1.92 check and dependency-policy check passed.
  Dependency policy reported allowed duplicate-version warnings.
- Recorded source-file and release-binary hashes matched the actual files
  immediately after both runs. The worktree was dirty; the recorded HEAD
  alone does not describe the candidate. Source hashes identify its files.

Pooled SQLx recorded 50,000 administrative commands per trial; native retain
recorded none. Native reset also re-prepared the statement per insert.
These are global counters, not packet traces; other clients can affect them.
The held control is stronger attribution evidence than those counters alone.

## Artifacts and limits

- [Pooled raw trials](paired.jsonl), [summary](paired-summary.json),
  [provenance](provenance.json).
- [Held raw trials](held.jsonl), [summary](held-summary.json),
  [provenance](held-provenance.json).
- [Short smoke trials](smoke.jsonl): validation evidence only; excluded from
  the reported performance results. Empty stderr files accompany valid runs.
- [Probe source and instructions](../../mysql_driver_probe/README.md).

This experiment excludes HTTP handling, validation, ORM plans, type decoding,
trigger-aware RETURNING, general transactions and migrations. The fixed
insert workload does not mutate session state; arbitrary raw SQL cannot
safely inherit that assumption. Mixed traffic, saturation at concurrency 20,
network RTT, production storage, restarts, TLS and full shutdown/cancellation
contracts still require testing. Native reset occupies the middle position
in paired ordering; Latin-square balancing remains a publication improvement.

No Siderite/FastAPI speedup or production native backend adoption is claimed.
