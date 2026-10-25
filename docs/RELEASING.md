# Releasing

## Checklist

1. Run the gates: `cargo fmt --check`,
   `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
   `cargo test --workspace --all-features`,
   `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`.
2. Run the live-database suites against `docker-compose.yml` (see
   [TESTING.md](TESTING.md)).
3. Check the MSRV: `cargo +1.99.0 check --workspace --all-features --all-targets`.
4. Run `cargo deny check`.
5. Move the `[Unreleased]` section of `CHANGELOG.md` under the new version
   and bump `workspace.package.version` plus the `version` of every internal
   dependency in the root `Cargo.toml`.
6. Tag `vX.Y.Z` and publish in the order below. The tag also publishes
   the prebuilt CLI (below).

## Prebuilt CLI

Pushing a `vX.Y.Z` tag runs `.github/workflows/release.yml`. It fails
unless the tag matches the `siderite-cli` version. It builds `siderite`
with the `mcp`, `postgres` and `mysql` features for each target, checks
that `siderite --version` prints that version and that `siderite new`
writes an app, then creates the GitHub release with notes generated from
the merged pull requests. A manual run of the workflow builds the same
archives as artifacts and publishes nothing.

| Target | Platform | Runner |
|---|---|---|
| `aarch64-apple-darwin` | macOS, Apple silicon | `macos-15` |

Each archive, `siderite-X.Y.Z-TARGET.tar.gz`, holds the binary,
`README.md` and both licenses, next to a `.sha256` file. The binary is ad
hoc signed by the linker, not signed with a Developer ID or notarized: an
archive fetched with `curl` runs as is, while one downloaded by a browser
is quarantined by Gatekeeper until `xattr -d com.apple.quarantine
siderite` clears it. To install it:

```bash
v=0.1.0 t=aarch64-apple-darwin
base=https://github.com/jraavis/siderite/releases/download/v$v
a=siderite-$v-$t.tar.gz
curl -fLO "$base/$a" -fLO "$base/$a.sha256"
shasum -a 256 -c "$a.sha256"
tar -xzf "$a"
install -m 755 "siderite-$v-$t/siderite" ~/.local/bin/
```

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
10. `siderite-sessions`
11. `siderite-cli`
12. `siderite-testkit`
13. `siderite`

`siderite-bench` and the examples are `publish = false`.

Several crates use `siderite-testkit` as a dev-dependency while the testkit
depends on them. Cargo does not build dev-dependencies when it verifies a
package, but the first release has not been published yet, so this order has
not been checked against crates.io.
