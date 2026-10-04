# Owned fixture correctness acceptance

Date: 2026-10-03. Release and source identity are recorded in metadata.json.
Ten isolated application trials exercised SQLite WAL/FULL, PostgreSQL,
SQLx MySQL, native MySQL and MongoDB. Each used 500 timed inserts,
concurrency 10 and one trial pair. Fixtures were newly generated private
server databases or a temporary SQLite file, then removed after both apps
stopped. All trials passed full committed-row deltas, exact-201 preflight
and independent returned-ID/title/done samples: 5,000 timed rows and 100
sampled inserts. MongoDB clients used the database named in the owned URL.

The raw trial records and per-workload comparison JSON are retained here.
No trial meets the ten-second/five-pair publication minimum. Every lead gate
is false and no performance advantage is established by these smoke checks.
The CLI smoke report separately verifies the default owned-fixture path.
