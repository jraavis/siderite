# Matched-worker MongoDB insert evidence

Five alternating pairs, one observed scheduler worker per application,
10 connections, concurrency 50, 500 warm-up requests, and 46,478 timed
requests per trial. Both applications used MongoDB 8.3.11 with matching
primary selection, effective local read concern, majority write concern,
journaling, retry policy, and pool settings captured in the trial data.

The paired geometric Siderite/FastAPI ratio was 1.217x, with a 95% bootstrap
interval of 1.160–1.264x. Both throughput and per-pair tail gates passed.
The result covers this local independent-commit insert workload only.
The source and binary provenance, settings, counts, and raw percentile data
are retained under `trials/`; see `report.md` and `run.log`.
