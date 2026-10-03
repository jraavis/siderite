---
title: Development
description: Clone, format, clippy, test, rustdoc, and the library-code rules.
---

```bash
git clone https://github.com/jraavis/siderite
cd siderite
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

MSRV is 1.99:

```bash
cargo +1.99.0 check --workspace --all-features --all-targets
```

Live databases: see [Testing](/siderite/guides/production/testing/). License and
advisory policy: `cargo deny check` (`deny.toml`).

## Library rules

- No `unwrap` / `expect` in library code (tests may use them). Clippy
  `unwrap_used` and `expect_used` are denied workspace-wide.
- No `unsafe`. Every crate declares `#![forbid(unsafe_code)]`.
- Document every public item. `cargo doc --workspace --no-deps --all-features`
  runs with `RUSTDOCFLAGS='-D warnings'` in CI.
- Backend limitations must fail explicitly with a capability error. Nothing
  is silently ignored.
- Never log bind parameters, passwords, tokens, API keys, or `Secret`
  values.
- Do not add `Co-Authored-By` or any AI attribution to commits, PRs, or
  signatures.

## This documentation site

Sources live in `website/`. From that directory:

```bash
bun install
bun run dev
```

The GitHub Pages workflow builds Starlight and copies `cargo doc` to
`/api/`. Internal links are checked by `starlight-links-validator`.

## See also

- [Releasing](/siderite/contributing/releasing/)
- [Benchmarks](/siderite/contributing/benchmarks/)
- [Crate map](/siderite/reference/crates/)
