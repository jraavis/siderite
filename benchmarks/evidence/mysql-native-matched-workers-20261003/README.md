# Native MySQL with matched workers and settings

Five alternating pairs on 2026-10-03 compared the experimental native
mysql_async adapter with FastAPI/aiomysql. Both used MySQL 8.4.11, pool size
10, concurrency 50, warm-up 500 and one observed scheduler worker. Sampled
autocommit=1, timezone=+00:00, SQL mode, redo-flush=1 and binlog-sync=1
matched. Local source/binary build provenance matched at startup.

Independent readers verified 565,540 timed committed rows and 500 untimed
response samples. Every timed trial exceeded 13 seconds.

The paired geometric throughput ratio was 1.012x, with a 95% whole-pair
bootstrap interval of 0.965–1.063x. Median throughput was 3,955 versus 3,854
requests/second. Neither adoption nor strong-lead gates passed; the interval
includes parity and a per-pair p99 regression exceeded the five-percent
budget. Raw CSV, row and response checks, settings and comparisons are
retained under `trials/`; the successful CLI report is `report.md`.

This result does not support promoting the native adapter. Earlier results
without observed matched worker counts cannot overturn it. A same-control
SQLx experiment and cost decomposition are needed to identify whether pool
retention, compilation, protocol, scheduling or server durability dominate.
No database service was restarted; the private fixture was removed.
