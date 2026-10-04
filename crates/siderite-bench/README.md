# siderite-bench

Criterion benchmarks for siderite (spec section 54). Not published.

```bash
cargo bench -p siderite-bench -- --warm-up-time 1 --measurement-time 2
```

## Benchmarks

- `routing`: dispatch through siderite's `RouterService` versus a raw axum
  `Router` with the same routes (a static route and a `{id}` path parameter),
  driven in process with `tower::ServiceExt::oneshot`.
- `extract`: a JSON echo handler using siderite's validating `Json<T>`
  (`#[derive(Validate, Schema, Deserialize)]` model) versus raw `axum::Json`;
  plus response serialization (`Json<T>` into a response through `Dump`
  with streaming serialization versus value-tree construction, raw
  `axum::response::IntoResponse`, and `serde_json::to_vec`).
- `orm`: `QuerySet` builder to `QueryPlan`, SQL compilation of that plan for
  SQLite and PostgreSQL, and fetching 100 rows from in-memory SQLite.

## Results

Numbers are machine-specific and only describe the run below. Do not compare
them across machines or treat them as guarantees.

Machine:

```
Darwin LN-2031.local 25.6.0 Darwin Kernel Version 25.6.0: Fri Jul 31 19:16:36 PDT 2026; root:xnu-12377.161.14~5/RELEASE_ARM64_T6030 arm64
rustc 1.99.0
CPU: Apple M3 Pro
```

Run with `--warm-up-time 1 --measurement-time 2` (release profile). Values are
Criterion's median point estimate per iteration.

| Benchmark | Median |
|---|---|
| `routing/siderite/static` | 1.93 us |
| `routing/axum/static` | 681 ns |
| `routing/siderite/path_param` | 2.13 us |
| `routing/axum/path_param` | 883 ns |
| `extract/siderite/json_validate_respond` | 4.33 us |
| `extract/axum/json_respond` | 2.01 us |
| `response_serialization/siderite/dump` | 350 ns |
| `response_serialization/siderite/value_tree_reference` | 790 ns |
| `response_serialization/axum/response` | 488 ns |
| `response_serialization/serde_json/to_vec` | 106 ns |
| `orm/queryset_build` | 1.28 us |
| `orm/compile/sqlite` | 453 ns |
| `orm/compile/postgres` | 482 ns |
| `orm/sqlite_fetch_100_rows` | 147 us |

Notes on what is measured:

- Routing and extract iterations include building the request and cloning the
  service, for both sides equally.
- `response_serialization/siderite/dump` streams response JSON directly without
  allocating an intermediate JSON tree. It outperforms raw Axum response
  building (`axum/response`) while preserving custom dump hooks and field filtering.
- `response_serialization/siderite/value_tree_reference` preserves the previous
  value-tree serialization algorithm as an in-run control.
- `serde_json/to_vec` serializes the raw model to bytes without constructing an HTTP response.
- `orm/sqlite_fetch_100_rows` runs `Widget::objects(&db).all()` against a
  single-connection in-memory SQLite database seeded with 100 rows.

## End-to-end HTTP benchmarks

Throughput against equivalent FastAPI apps, measured with ApacheBench on the
same machine (median of 3 runs, 0 failures), is documented in
[`docs/BENCHMARKS.md`](../../docs/BENCHMARKS.md) and on the website's
[benchmarks page](https://jraavis.github.io/siderite/contributing/benchmarks/):
plain-HTTP routes from `examples/hello_world` plus a minimal Todo API on
SQLite, PostgreSQL, MySQL and MongoDB. Those automated harnesses live in
[`benchmarks/`](../../benchmarks/) and are driven via `python3 benchmarks/run_benchmarks.py`.
