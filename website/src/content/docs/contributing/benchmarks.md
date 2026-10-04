---
title: Benchmarks
description: How to run Criterion benches and end-to-end HTTP benchmarks, and how to read the numbers.
---

Criterion benchmarks live in the `siderite-bench` crate (not published). Its
README, `crates/siderite-bench/README.md`, lists what each benchmark
measures and the latest snapshot with the machine they were taken on.

```bash
cargo bench -p siderite-bench -- --warm-up-time 1 --measurement-time 2
```

Run a single group by name, for example `cargo bench -p siderite-bench -- routing`.
The groups are `routing`, `extract`, and `orm`. Criterion writes HTML
reports to `target/criterion`.

Numbers are machine-specific. Compare runs only on the same machine, and
treat the README’s table as a snapshot, not a guarantee.

| Group | Measures |
|---|---|
| `routing` | dispatch through siderite’s `RouterService` versus a raw axum `Router` |
| `extract` | validating `Json<T>` versus raw `axum::Json`; `Dump` versus `serde_json::to_vec` |
| `orm` | QuerySet → QueryPlan, SQL compilation (SQLite and PostgreSQL), fetch of 100 SQLite rows |

## HTTP benchmarks versus FastAPI

End-to-end throughput measured with ApacheBench (`ab -l`) on the same
machine, one server at a time (DB rounds) or side by side on different ports
(plain-HTTP round). Siderite runs a `--release` build; FastAPI runs under
Uvicorn with a single worker, no reload, and warning log level. Automated benchmark harnesses
live in `benchmarks/`.

Machine: Darwin arm64 (Apple M3 Pro), rustc 1.99.0, Python 3.13.14,
FastAPI 0.142.1, Uvicorn 0.54.0. Databases run in local containers: PostgreSQL 17,
MySQL 8.4, MongoDB 8 (single-node replica set).

### Plain HTTP (`examples/hello_world`)

Direct streaming response serialization without intermediate JSON tree construction:

| Test | Siderite (req/s) | FastAPI (req/s) | Speedup | Latency p99 (S / F) |
|---|---|---|---|---|
| `GET /` | 36,701 | 18,441 | 2.08x | 3.5 ms / 3.6 ms |
| `GET /hello/{name}` | 37,884 | 12,692 | 2.98x | 3.3 ms / 5.8 ms |
| `POST /echo` (JSON) | 24,084 | 13,510 | 2.00x | 3.2 ms / 3.3 ms |

### Database-backed Todo API

A minimal Todo API (`id`, `title`, `done`) with `GET /todos` (latest 20),
`GET /todos/{id}`, and `POST /todos` → 201, implemented once with the
siderite ORM (`SqliteBackend`, `PgBackend`, `MySqlBackend`, `MongoBackend`, matching pool of 10) and
once with FastAPI (`sqlite3`, `asyncpg`, `aiomysql`, `motor`, matching pool of 10). Tables are
truncated and reseeded with 100 rows before each phase.

| DB | Test | Siderite (req/s) | FastAPI (req/s) | Speedup | Latency p99 (S / F) | Notes |
|---|---|---|---|---|---|---|
| SQLite (default) | list 20 | 18,108 | 1,709 | 9.10x | 2.3 ms / 28.1 ms | Rollback journal, pool 10 |
| SQLite (default) | get one | 23,592 | 5,598 | 4.37x | 2.7 ms / 11.4 ms | Single row lookup |
| SQLite (default) | insert | 2,715 | 1,286 | 2.00x | 5.4 ms / 150.3 ms | In-process `WriteGate` eliminates busy-sleep |
| SQLite (group commit) | insert | 8,589 | 1,457 | 5.59x | 3.7 ms / 120.1 ms | `--siderite-group-commit` shares commit sync |
| SQLite (WAL) | list 20 | 17,406 | 1,705 | 10.62x | 2.8 ms / 24.4 ms | `--sqlite-wal` |
| SQLite (WAL) | get one | 20,818 | 6,052 | 3.68x | 3.2 ms / 8.2 ms | |
| SQLite (WAL) | insert | 14,508 | 4,414 | 3.31x | 1.9 ms / 27.7 ms | |
| PostgreSQL 17 | list 20 | 9,583 | 7,606 | 1.12x | 6.1 ms / 9.0 ms | Pool 10 |
| PostgreSQL 17 | get one | 10,723 | 8,422 | 1.01x | 3.5 ms / 8.0 ms | |
| PostgreSQL 17 | insert | 8,546 | 5,957 | 1.42x | 1.7 ms / 2.7 ms | |
| MySQL 8.4 | list 20 | 10,473 | 5,089 | 2.04x | 3.7 ms / 15.6 ms | Pool 10 |
| MySQL 8.4 | get one | 9,886 | 7,386 | 1.39x | 3.7 ms / 4.7 ms | |
| MySQL 8.4 | insert | 3,371 | 3,433 | 1.02x | 6.5 ms / 5.9 ms | |
| MongoDB 8 | list 20 | 10,729 | 3,825 | 2.92x | 5.5 ms / 10.2 ms | Replica set |
| MongoDB 8 | get one | 13,049 | 4,116 | 3.05x | 3.5 ms / 11.5 ms | |
| MongoDB 8 | insert | 2,970 | 2,580 | 1.20x | 6.8 ms / 7.2 ms | |

### Optimization Highlights

- **SQLite write gate:** Siderite's `WriteGate` serializes write requests across pool connections in-process via a FIFO queue. This avoids SQLite file lock races where losing connections sleep in SQLite's busy handler (exponential backoff up to 100 ms), dropping p99 tail latency from ~150 ms to **5.4 ms**.
- **SQLite group commit:** The opt-in `SqliteBackend::group_commit` batches concurrent autocommit writes into a single transaction and commit sync, driving insert throughput to **8,589 req/s** (**5.59x** speedup).
- **SQLite WAL mode:** In write-ahead log mode, readers and writers operate concurrently without blocking, reaching **14,508 req/s** (**3.31x** speedup) at **1.9 ms** p99.

## See also

- [Development](/siderite/contributing/development/)
- [Architecture](/siderite/internals/architecture/)
