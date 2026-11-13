# CI/CD

| Workflow | Trigger | What it does |
|---|---|---|
| `ci.yml` | PRs, pushes to `master`, manual, called by `release.yml` | Lint, tests, live databases, journey, MSRV, `cargo deny`, Python, workflow lint |
| `pr-title.yml` | PR opened or edited | PR title follows Conventional Commits |
| `pages.yml` | `website/**` or `crates/**` on `master` | Builds the site and rustdoc, deploys GitHub Pages |
| `scheduled.yml` | Daily 03:17 UTC, manual | Advisories, beta toolchain, feature powerset, minimal versions, coverage |
| `release.yml` | `v*` tag, manual | CI on the tag, Linux and macOS binaries, GitHub release, crates.io |

## Pull requests

`ci.yml` starts with a `changes` job: on a pull request, each group of
jobs runs only when its paths changed (`.github/path-filters.yml`), so a
website-only PR skips the Rust jobs. Pushes to `master`, tags and manual
runs always run everything. `ci-success` depends on every job and fails if
any failed or was cancelled; it is the one CI check branch protection
needs.

Pull requests share no cache writes: only `master` saves the Cargo cache
(`.github/actions/setup-rust`), so a PR cannot evict or poison it. Release
builds use no cache at all.

## Supply chain

- Every third-party action is pinned to a full commit SHA with its version
  in a comment; Dependabot (`.github/dependabot.yml`) proposes updates
  weekly, a week after each release.
- Workflows default to a read-only token; jobs ask for more only where
  they need it. Checkouts do not keep credentials.
- `workflow-lint` runs actionlint (with shellcheck) and zizmor on any
  change under `.github/`.
- Release archives carry signed SLSA build provenance. Verify one with:

```bash
gh attestation verify siderite-0.1.0-x86_64-unknown-linux-musl.tar.gz --repo ssraavi/siderite
```

## Coverage

The daily `coverage` job writes a per-crate summary to the run page and
keeps `lcov.info` as an artifact for 30 days. Nothing is sent to a third
party.

## One-time repository settings

These live in GitHub settings, not in the repository.

1. **Settings → Secrets and variables → Actions → Variables**: create
   `AUTORUN` = `true`. Without it, pushes and PRs do not run CI (manual
   runs and tags still do). The repository is public, so standard runners
   cost nothing.
2. **Settings → Rules → Rulesets → New branch ruleset** for `master`:
   require a pull request, require the status checks `ci-success` and
   `conventional-commits`, block force pushes, and allow squash merging
   only.
3. **Settings → Environments → New environment** `crates-io`: add
   yourself as a required reviewer and limit deployment to tags matching
   `v*`. See [RELEASING.md](RELEASING.md#cratesio).
4. **Settings → Pages**: source "GitHub Actions" (already used by
   `pages.yml`).
5. **Settings → Code security**: enable Dependabot alerts and security
   updates, secret scanning and push protection.
