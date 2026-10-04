# SQLx MySQL control with matched workers and settings

Five alternating pairs used MySQL 8.4.11, pool size 10, concurrency 50,
warm-up 500 and one observed scheduler worker per application. Effective
SQL version, autocommit, timezone, mode and redo/binlog durability settings
matched. The current source/binary provenance matched at run startup.

The paired ratio was 0.923x FastAPI, with a 95% whole-pair bootstrap interval
of 0.894–0.959x. Median throughput was 3,574 versus 3,799 requests/second.
Neither adoption nor strong-lead gates passed. Raw trial checks, precise
percentiles, settings, worker counts and comparison reasons are retained in
`trials/`; the successful CLI summary is `report.md`.

Native MySQL's separate matched-control experiment measured 1.012x FastAPI.
That suggests improvement over SQLx but is not a direct paired native/SQLx
confidence interval: the cohorts ran at different times. Keep the native
adapter experimental and perform a direct driver comparison before making
an adoption claim. The fixture was private and was removed after the run.
