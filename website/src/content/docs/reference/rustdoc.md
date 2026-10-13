---
title: API rustdoc
description: Hosted rustdoc for every workspace crate.
---

Every public item is documented. CI builds rustdoc with
`RUSTDOCFLAGS='-D warnings'` and publishes it next to this site.

**Crate rustdoc:** [siderite API](/siderite/api/siderite/) (generated on deploy).

Until crates.io publishes the first release, this hosted rustdoc is the
API reference. After publish, docs.rs will carry the same docs per
version.

| Crate | rustdoc |
|---|---|
| `siderite` | [/api/siderite/](/siderite/api/siderite/) |
| `siderite-core` | [/api/siderite_core/](/siderite/api/siderite_core/) |
| `siderite-validation` | [/api/siderite_validation/](/siderite/api/siderite_validation/) |
| `siderite-orm` | [/api/siderite_orm/](/siderite/api/siderite_orm/) |
| `siderite-backends` | [/api/siderite_backends/](/siderite/api/siderite_backends/) |
| `siderite-macros` | [/api/siderite_macros/](/siderite/api/siderite_macros/) |
| `siderite-openapi` | [/api/siderite_openapi/](/siderite/api/siderite_openapi/) |
| `siderite-migrations` | [/api/siderite_migrations/](/siderite/api/siderite_migrations/) |
| `siderite-config` | [/api/siderite_config/](/siderite/api/siderite_config/) |
| `siderite-cache` | [/api/siderite_cache/](/siderite/api/siderite_cache/) |
| `siderite-sessions` | [/api/siderite_sessions/](/siderite/api/siderite_sessions/) |
| `siderite-cli` | [/api/siderite_cli/](/siderite/api/siderite_cli/) |
| `siderite-testkit` | [/api/siderite_testkit/](/siderite/api/siderite_testkit/) |

Build it locally:

```bash
cargo doc --workspace --no-deps --all-features --open
```

On this GitHub Pages site the same output is copied to `/api/` after the
Starlight build. The rustdoc pages are not in the Starlight sidebar; they
use rustdoc’s own navigation.

## See also

- [Crate map](/siderite/reference/crates/)
- [Prelude](/siderite/reference/prelude/)
