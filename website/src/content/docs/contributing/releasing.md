---
title: Releasing
description: Release checklist and crate publish order.
---

## Checklist

1. Run the gates: `cargo fmt --check`,
   `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
   `cargo test --workspace --all-features`,
   `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`.
2. Run the live-database suites against `docker-compose.yml` (see
   [Testing](/siderite/guides/production/testing/)).
3. Check the MSRV: `cargo +1.99.0 check --workspace --all-features --all-targets`.
4. Run `cargo deny check`.
5. Move the `[Unreleased]` section of `CHANGELOG.md` under the new version
   and bump `workspace.package.version` plus the `version` of every internal
   dependency in the root `Cargo.toml`.
6. Tag `vX.Y.Z` and publish in the order below.

## Publish order

Each crate depends only on crates listed before it (normal dependencies):

1. `siderite-config`
2. `siderite-macros`
3. `siderite-validation`
4. `siderite-openapi`
5. `siderite-orm`
6. `siderite-backends`
7. `siderite-core`
8. `siderite-migrations`
9. `siderite-cache`
10. `siderite-cli`
11. `siderite-testkit`
12. `siderite`

`siderite-bench` and the examples are `publish = false`.

Several crates use `siderite-testkit` as a dev-dependency while the testkit
depends on them. Cargo does not build dev-dependencies when it verifies a
package.

## See also

- [Development](/siderite/contributing/development/)
- [Crate map](/siderite/reference/crates/)
