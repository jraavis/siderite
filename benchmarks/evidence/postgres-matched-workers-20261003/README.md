# PostgreSQL SQLx insert evidence with matched controls

Five alternating pairs used PostgreSQL 17.11, pool size 10, concurrency 50,
warm-up 500 and one observed scheduler worker per application. Both
applications sampled synchronous_commit=on, fsync=on, full_page_writes=on
and timezone=UTC. Source/binary provenance matched at startup.

The paired geometric throughput ratio was 1.231x FastAPI, with a 95%
whole-pair bootstrap interval of 1.179–1.286x. Median throughput was 8,343
versus 6,862 requests/second; median p99 was 16.357 versus 28.964 ms. Every
pair met the conservative tail budget. Adoption and strong-lead gates
passed. This experiment retained SQLx; no PostgreSQL driver rewrite was
needed to establish this local insert result.

Independent readers verified 1,152,100 timed committed rows and 500 untimed
response samples. Raw outputs, percentile CSV, settings, worker counts,
row checks, build provenance and pair comparisons are retained in `trials/`.
The fixture was private and was removed after the successful CLI run.

This is one host and one independent-commit insert workload. Physical pool
warm-up coverage, total CPU/storage budgets, durability fault testing,
additional workloads and cross-host replication remain acceptance work.
The result does not establish a universal performance lead.
