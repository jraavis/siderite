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
- Rust files stay below 1,000 lines and new code stays within 79 columns.
  `python3 scripts/check_size.py` (run in CI) fails on any new file of 1,000
  lines or more and on growth of the existing per-file exceptions in
  `scripts/size_baseline.json`. Line width is a per-package ratchet (package =
  `crates/<name>`, `examples/<name>` or `benchmarks/<name>`): the number of
  lines over 100 columns (rustfmt's `max_width`) and the number over 79 columns
  may not grow in any package, so splitting a file within a package never
  trips the guard. Packages not in the baseline have a budget of zero.
  Existing exceptions may only shrink; after splitting a file or wrapping
  lines run `python3 scripts/check_size.py --update` to ratchet the baseline
  down. Exit status is 1 for violations and 2 for usage or I/O errors.
- The CLI packages a search index of the website docs and examples
  (`crates/siderite-cli/src/docs/index.json`). After changing either, run
  `SIDERITE_UPDATE_DOCS_INDEX=1 cargo test -p siderite-cli docs` and commit
  the regenerated file; a test fails while it is stale.
- New dependencies pass `cargo deny check` and build on the MSRV.

PRs are squash-merged; the PR title becomes the commit subject.

The full contributor guide is on GitHub Pages:
[Development](https://jraavis.github.io/siderite/contributing/development/).

Documentation site sources are in `website/`. From that directory: `bun install && bun run dev`.

