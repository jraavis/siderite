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
6. Push the tag `vX.Y.Z`. `.github/workflows/release.yml` does the rest:
   it re-runs CI on the tag, publishes the prebuilt CLI (below), then
   waits for approval of the `crates-io` environment and publishes the
   crates (see [crates.io](#cratesio)).

## Prebuilt CLI

Pushing a `vX.Y.Z` tag runs `.github/workflows/release.yml`. It fails
unless the tag matches the `siderite-cli` version and `CHANGELOG.md` has a
`## [X.Y.Z]` section. It re-runs the whole CI suite on the tagged commit,
builds `siderite` with the `mcp`, `postgres` and `mysql` features for each
target from a clean state (no cache), checks that `siderite --version`
prints that version and that `siderite new` writes an app, attests the
build provenance of each archive, then creates the GitHub release with
that changelog section as its notes. A version with a pre-release suffix
(`0.2.0-rc.1`) is marked as a pre-release. A manual run of the workflow
builds the same archives as artifacts and publishes nothing.

| Target | Platform | Runner |
|---|---|---|
| `x86_64-unknown-linux-musl` | Linux x86-64, static | `ubuntu-26.04` |
| `aarch64-unknown-linux-musl` | Linux ARM64, static | `ubuntu-26.04-arm` |
| `x86_64-apple-darwin` | macOS, Intel | `macos-15-intel` |
| `aarch64-apple-darwin` | macOS, Apple silicon | `macos-15` |

The Linux binaries are statically linked against musl and run on any
distribution. Verify where an archive came from with
`gh attestation verify <archive> --repo ssraavi/siderite`.

Each archive, `siderite-X.Y.Z-TARGET.tar.gz`, holds the binary,
`README.md` and both licenses, next to a `.sha256` file. The binary is ad
hoc signed by the linker, not signed with a Developer ID or notarized: an
archive fetched with `curl` runs as is, while one downloaded by a browser
is quarantined by Gatekeeper until `xattr -d com.apple.quarantine
siderite` clears it. To install it:

```bash
v=0.1.0 t=aarch64-apple-darwin  # or another target above
base=https://github.com/jraavis/siderite/releases/download/v$v
a=siderite-$v-$t.tar.gz
curl -fLO "$base/$a" -fLO "$base/$a.sha256"
shasum -a 256 -c "$a.sha256"
tar -xzf "$a"
install -m 755 "siderite-$v-$t/siderite" ~/.local/bin/
```

## crates.io

The `crates-io` job of the release workflow runs
`scripts/publish_crates.py --execute`, which:

- checks every publishable crate on crates.io first and stops before
  uploading anything if a name already belongs to another repository;
- skips versions that are already published, so a failed run can be
  re-run;
- runs `cargo publish --workspace --locked`, which orders the crates by
  their dependencies.

A manual run of the workflow runs the same script as a dry run
(`cargo publish --dry-run`). Locally: `python3 scripts/publish_crates.py`.

### Authentication: trusted publishing

The workflow authenticates with crates.io
[trusted publishing](https://crates.io/docs/trusted-publishing): GitHub
issues the job a short-lived OIDC token, `rust-lang/crates-io-auth-action`
exchanges it for a crates.io token that expires after 30 minutes, and no
long-lived API token is stored anywhere. crates.io only accepts a trusted
publisher for a crate that already exists, so the first release uses a
temporary API token once.

One-time setup:

1. Sign in to [crates.io](https://crates.io) with GitHub and verify your
   email address under Account Settings (publishing requires it).
2. In GitHub, **Settings → Environments → New environment** `crates-io`:
   add yourself as a required reviewer and, under deployment branches and
   tags, allow only tags matching `v*`.
3. **First release only.** On crates.io, Account Settings → API Tokens →
   New Token: scopes `publish-new` and `publish-update`, expiry 7 days.
   Add it as the **environment** secret `CARGO_REGISTRY_TOKEN` of
   `crates-io` (not as a repository secret). Push the tag and approve the
   `crates-io` job when it asks.
4. For each published crate, open
   `https://crates.io/crates/<crate>/settings`, choose **Trusted
   Publishing → Add**, and enter: owner `ssraavi`, repository
   `siderite`, workflow `release.yml`, environment `crates-io`.
5. Delete the `CARGO_REGISTRY_TOKEN` secret and revoke the token on
   crates.io. Later releases use trusted publishing only: the workflow
   falls back to the secret only while it exists.
6. Optional, once every crate has a trusted publisher: in each crate's
   settings, require trusted publishing so API tokens can no longer
   publish it.

### Package name: `siderite-web`

The crate name `siderite` on crates.io belongs to an unrelated project
(`maugier/siderite`, 2021). The facade (`crates/siderite`) is therefore
published as `siderite-web`, and its library keeps the name `siderite`:

```toml
[package]
name = "siderite-web"

[lib]
name = "siderite"
```

Applications depend on `siderite-web = "X.Y"` and still write
`use siderite::...`. The CLI recognizes apps by a normal dependency on
the `siderite-web` package (`crates/siderite-cli/src/workspace.rs`).

### Publish order

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
13. `siderite-web`

`siderite-bench` and the examples are `publish = false`.

Several crates use `siderite-testkit` (and `siderite-cli` uses `siderite-web`)
as a dev-dependency while those crates depend on them. Those
dev-dependencies are path-only, without a version, so Cargo strips them
from the published manifests and the cycle never reaches crates.io;
`cargo publish --workspace --dry-run` packages all thirteen crates in the
order above. Keep any new dev-dependency on a crate published later in
this order path-only too.
