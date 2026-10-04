# SQLite WAL/FULL single-lease insert evidence

Five alternating paired trials on 2026-10-03 compared the release Siderite
SQLx SQLite adapter with FastAPI's retained native SQLite connection.
Both used WAL/FULL, statement autocommit, pool size one, concurrency 20,
500 warm-up requests and one observed scheduler worker. Each timed trial
sent 205,096 inserts; all ten lasted at least 14.127 seconds.

Independent readers verified 2,050,960 timed committed rows. Another 200
untimed samples verified status 201, unique returned IDs and stored rows.
Median throughput was 14,322 versus 8,551 requests/second. The geometric
paired ratio was 1.67755x; 10,000 whole-pair bootstrap draws, seed zero,
produced a 95% interval of 1.62966–1.74518x.

All Siderite p99 values were 2 ms; all FastAPI values were 3 ms. The original
AB console values round to whole milliseconds. The retrospective
`rounding-aware-comparison.json` includes conservative ±0.5 ms bounds:
each pair's p99 ratio upper bound is 1.0, within the 1.05 budget. Both local
adoption and strong-lead gates pass. The run preceded CSV percentile capture;
do not describe its tails as submillisecond measurements.

`report.md`, `run.log`, `trials/` and the rounding-aware comparison retain
results, startup worker evidence, manifests, raw outputs and row checks.
A preceding run overlapped compilation and is excluded in the separate
`interrupted-build-overlap-20261003` directory. No compilation or indexing
ran during this replacement experiment.

These results establish one local insert cell. FastAPI's connection executes
blocking SQLite calls in its event loop. Reader/mixed workloads and a pooled
worker comparator are required before broader claims. Effective Rust PRAGMA
readback, total CPU budgets, storage durability under faults, and reproducible
source-to-binary build provenance remain acceptance work. Configured sync
policy and successful committed-row checks do not establish those properties.


A later startup-settings smoke found the default Rust/Python artifacts use
SQLite 3.46.0 and 3.53.4 respectively. These earlier trials did not capture
engine versions, so they cannot certify matched-engine performance. Keep
this ratio as local historical evidence; do not promote it to a controlled
framework lead. A same-library rerun is required.
