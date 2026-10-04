# Concurrent insert correctness smoke evidence

Collected on 2026-10-03 using the existing release Todo application and
FastAPI, one pair per profile, fresh HTTP connections, 500 timed requests,
concurrency 10 and pool size 10. Profiles: SQLite, PostgreSQL, SQLx MySQL
and native MySQL. These are short correctness checks, not speed evidence.

Eight timed trials each passed a 500-row committed count check. After each
valid trial, ten separate concurrent requests passed exact HTTP 201, JSON
values, unique positive IDs and an independent batch stored-row check.
The 80 sample writes are excluded from the 4,000 timed committed inserts.

Trial JSON with `response_checks` contains final validation evidence. Other
JSON records retain raw load-generator runs and warm-up evidence. The sample
field explicitly states that measured responses were not individually
checked. Application logs append every launch. `summary.json` was computed
from final records with response checks; `smoke-results.json` contains the
runner output, whose rates are not publication-ready comparisons.

SQLite used a temporary file. MySQL and PostgreSQL used unique databases
that were dropped after the checks. Shared services were not restarted.
MongoDB was not exercised live. No full durability, storage, release-build
or effective-settings provenance was captured for these smoke checks.
