# Native MySQL HTTP insert diagnostic

Measured 2026-10-03 on the local development host against MySQL 8.4.11.
This is a diagnostic, not publication-ready or an all-database claim.

Each driver profile has five alternating Siderite/FastAPI pairs, concurrency
20, pool size 10, fully initialized pools, fresh TCP connections and at least
10 seconds per measured trial. One POST independently commits one row.
The native series ran first; the SQLx series followed. Do not treat those
separate series as a paired native-versus-SQLx experiment.

| Profile | Median rps S/F | Paired gain | Bootstrap 95% interval |
| --- | --- | ---: | --- |
| Native | 4,431.86 / 3,896.93 | 1.127x | 1.073x–1.184x |
| SQLx | 3,691.99 / 4,025.11 | 0.863x | 0.745x–0.935x |

Median p99 latency was 12/15 ms for native/FastAPI and 16/13 ms for
SQLx/FastAPI.

The ratio of median throughputs is different from the paired geometric
ratio. Both are descriptive; the adoption gate uses paired evidence.
The native interval's lower bound is below 1.10x, so this run does not
establish the proposed reliable 10% minimum advantage. Five pairs also
provide limited uncertainty estimates. The SQLx series includes a slow
second trial (2,408.07 rps); it remains in the evidence rather than being
discarded. Host contention and between-series drift remain limitations.

All 20 timed trials completed their calibrated request counts with no
reported failures/non-2xx responses and independently checked committed row
deltas: 575,150 native-series rows and 576,340 SQLx-series rows. Exact 201
status and persisted-row checks run before each trial, but ApacheBench does
not prove every measured response's status/body/ID. Full response sampling
and cross-response ID validation remain publication gates. Application logs
have no traceback/panic/error/exception markers; that scan is not a complete
structured server-error validator. Logs append all launches with start-time
markers at the same path; trial identity and phase are not explicitly tagged
on each log segment.

`native/` and `sqlx/` preserve raw ApacheBench output, parsed validity and
row counts, calibration/warm-up records, application logs and manifests.
`summary.json` records all five pair ratios, 10,000 percentile bootstrap
draws with seed 20261003 and latency summaries. Bootstrap resamples pairs
and averages their log throughput ratios before exponentiating.
`provenance.json` records execution order, build command, runtime and
post-run database globals. These settings were not captured from each
measured client session. Manifests include dirty-source and binary hashes;
they do not prove the complete effective build environment.

Post-run server globals showed flush-on-commit 1, sync-binlog 1, binlog on,
doublewrite on and repeatable-read. The orchestrator dropped its uniquely
owned database after both profiles. No shared service was restarted.

An earlier keep-alive smoke run is excluded: ApacheBench negotiated retained
connections only for Siderite under HTTP/1.0. The harness now rejects that
asymmetry. These diagnostic profiles both use fresh connections.

Next evidence: independent repeat on a controlled host; interleaved direct
native/SQLx controls; exact response sampling; complete session, storage,
build and per-trial log provenance; TLS/recovery/migration contracts; reads
and mixed traffic across the declared database matrix.
