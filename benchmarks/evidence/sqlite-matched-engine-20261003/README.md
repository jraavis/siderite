# Matched-engine SQLite WAL/FULL insert evidence

Five alternating pairs on 2026-10-03 used SQLite 3.53.4 in both applications.
Siderite retained SQLx; its SQLite dependency linked the same installed
native library as Python through the recorded pkg-config build override.
Production's default bundled engine was not changed.

Both applications reported WAL, synchronous=2, foreign_keys=1 and
busy_timeout=5000. Pool size was one, concurrency 20, warm-up 500, and
observed scheduler workers were one in every process. Each timed trial sent
170,544 requests and lasted at least 13.053 seconds. Independent readers
verified 1,705,440 timed committed inserts plus 200 untimed response samples
with exact status 201, unique IDs and matching stored rows.

Median throughput was 12,086 versus 7,735 requests/second. The paired
geometric ratio was 1.57403x. Ten thousand whole-pair bootstrap draws,
seed zero, produced a 95% percentile interval of 1.50112–1.66445x.
All five conservative p99 ratio upper bounds were below 0.817 and met the
1.05 budget. Fine-resolution AB percentile CSV is retained, with 0.001 ms
quantization accounted for. Local adoption and strong-lead gates passed.

`trials/` contains source/binary digests, matching local build provenance,
raw outputs, percentile CSV, sampled effective settings, worker records,
response checks and per-pair comparisons. `run.log` and `report.md` retain
the complete successful CLI run. No compilation or indexing ran during
this experiment, and the owned temporary database was cleaned up.

This establishes one host and one insert profile. The FastAPI comparator
uses one retained native SQLite connection with blocking calls on its event
loop. Reader/mixed workloads and a pooled worker comparator remain required.
Settings are sampled, not proof of every physical connection. CPU/storage
budgets, fault durability, hermetic builds and independent host replication
remain acceptance work; this is not a lead across all databases.
