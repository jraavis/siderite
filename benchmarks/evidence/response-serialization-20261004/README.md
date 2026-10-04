# JSON response rewrite — 2026-10-04

The ordinary-model response path now encodes borrowed fields directly into
JSON bytes. It avoids the intermediate `serde_json::Value` object tree.
Nested custom dumps, hooks, filters, and numeric error behavior retain their
existing semantics. The complete byte body is buffered before publication.

The previous algorithm is retained as `siderite/value_tree_reference` in the
same benchmark executable, using the same model clone and HTTP response.
Criterion estimates from 100 samples, one-second warm-up and three-second
measurement per case:

| Response construction | Estimate | Criterion interval |
|---|---:|---:|
| Previous value-tree algorithm | 773.25 ns | 771.12–775.32 ns |
| Rewritten ordinary-model path | 361.80 ns | 359.81–363.87 ns |
| Axum response construction | 461.38 ns | 460.31–462.51 ns |
| Bare Serde byte encoding | 90.30 ns | 90.05–90.54 ns |

The rewritten path used 53.2% less time than the same-run old algorithm
(2.14x operation throughput). Bare Serde excludes the model clone and HTTP
response work, so its number is not an equivalent full-response comparison.
Axum also constructs different response headers; this does not establish
an overall framework lead over Axum.

Forty targeted tests passed: 36 existing validation/HTTP tests and four new
checks for nested custom dumps/hooks, filters, floating-point/wide-integer
semantics, and rejection of partially serialized successful responses.
Strict release library clippy passed for validation, macros, core, and facade.
Formatting and diff checks passed. Full all-feature release gates were not
rerun for this change; the all-target clippy attempt was cancelled while
waiting behind another workspace build.

Another workspace build was present during timing. Treat these as local
microbenchmark diagnostics, not isolated-host release-gate evidence. No new
SQL throughput result is claimed. Logs, samples, and source hashes are kept
beside this file. The earlier separate before/after timing was not used for
this comparison.
