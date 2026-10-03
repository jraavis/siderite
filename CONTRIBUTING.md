# Contributing

Before you open a PR, run:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Rules:

- No `unwrap`/`expect` in library code.
- No `unsafe`.
- Document every public item.
- Backend limitations must fail explicitly with a capability error.
- Never add `Co-Authored-By` or any AI attribution to commits, PRs, or signatures.

## Pull requests

Branch from `master`; never push to it directly. A PR is merged only when:

- All CI jobs pass: `lint` (fmt, clippy, rustdoc with `-D warnings`), `test`,
  `live` (Postgres, MySQL, MongoDB, Redis), `msrv` (Rust 1.99) and `deny`.
- It does one thing. Split unrelated changes into separate PRs.
- The title and commits follow [Conventional Commits](https://www.conventionalcommits.org/)
  (`feat:`, `fix:`, `docs:`, `test:`, `ci:`, `chore:`, with an optional scope,
  e.g. `fix(validation): ...`).
- The description says what changed and why, and links any related issue.
- New behavior and bug fixes come with tests. Compile-fail cases go in the
  trybuild UI tests (`crates/siderite/tests/ui`).
- Tests that need a live database are `#[ignore]`d so `cargo test` runs without
  services; CI runs them with `--include-ignored`.
- User-visible changes add an entry to `CHANGELOG.md`, and public API changes
  update `docs/` and `website/`.
- New dependencies pass `cargo deny check` and build on the MSRV.

PRs are squash-merged; the PR title becomes the commit subject.

The full contributor guide is on GitHub Pages:
[Development](https://jraavis.github.io/siderite/contributing/development/).

Documentation site sources are in `website/`. From that directory: `bun install && bun run dev`.

