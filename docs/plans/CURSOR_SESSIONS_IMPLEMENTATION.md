# Cursor pagination and sessions/CSRF: implementation plan

Updated: 2026-10-13. Implements the designs in
[CURSOR_PAGINATION.md](CURSOR_PAGINATION.md) (F02) and
[SESSIONS_CSRF.md](SESSIONS_CSRF.md) (F08). Those documents stay the
source of truth for semantics; this one fixes the API, file layout,
task order and acceptance checks.

Common rules for every task: no `unwrap`/`expect`/`unsafe` in library
code, every public item documented, files under 1,000 lines, new code
within 79 columns (`python3 scripts/check_size.py` must pass), backend
limitations fail with a capability error before any query, and each
user-visible change adds a `CHANGELOG.md` entry plus `docs/` and
`website/` updates.

## F02 — Cursor pagination

### P1 — Token codec (`siderite-orm`)

File: `crates/siderite-orm/src/queryset/cursor/token.rs`.

- Layout exactly as the design and the contract test: unpadded base64url
  of version `1`, fingerprint (`u64` big-endian), direction byte (`a`
  or `b`), then per ordering term a tag and value. Tags: `0` null, `1`
  i64 BE, `2` text (u32 BE length + UTF-8), `3` f64 BE bits, `4` bool
  (one byte, 0/1), `5` bytes (u32 length + bytes), `6` timestamp (i64
  microseconds since the epoch, UTC), `7` date (i32 days from
  1970-01-01), `8` uuid (16 bytes), `9` decimal as text (u32 length +
  ASCII), `10` time (i64 microseconds since midnight). Tags `0`-`2` are
  the contract test's encoding, byte for byte.
- `CursorError` with a stable `code()`: `Malformed` (`cursor.E001`: not
  base64url, longer than 1 KiB before decoding, truncated, trailing
  bytes, unknown tag, invalid UTF-8, or a direction byte that does not
  match the parameter it arrived in), `UnknownVersion` (`E002`),
  `ForeignQuery` (`E003`), `Mismatch` (`E004`: wrong value count, or a
  non-null value whose tag does not fit the term's `SqlType`).
- Checks run in the order of the contract test so each fixture yields
  exactly its code. Decoding allocates at most the token's length.

### P2 — Fingerprint and total ordering

File: `crates/siderite-orm/src/queryset/cursor/mod.rs`.

- Ordering terms: the queryset's `order_by` terms (or the model's
  `ordering`), each of which must be a plain column of `M` (resolve via
  `ModelMeta`; annotations, expressions, joins and JSON columns are an
  `InvalidPlan` error). Append the primary key ascending unless present.
- Fingerprint: the first 8 bytes of SHA-256 (`sha2`, already a workspace
  dependency) over a canonical byte string of the model table, the
  `Debug` form of the plan's `where` predicate (with bound values) and
  the resolved ordering terms. It excludes `limit`, `offset`, the seek
  predicate and `select_related`/`prefetch_related`. The `Debug` form
  ties tokens to a siderite version; document that an upgrade may turn
  old tokens into `cursor.E003`, which clients handle by restarting.
- Reject querysets the seek cannot express: `distinct`, grouping,
  `having`, compound (union) plans, an explicit `limit`/`offset`, and
  locks. `InvalidPlan` errors, before any query.

### P3 — Seek predicate, null placement and the fetch

- Build the expanded OR predicate from the design with existing `Expr`
  builders (no new plan nodes, no row-value comparison).
- Null placement without new syntax: for a nullable term emit
  `col IS NULL` as an extra sort key before the column (ascending for an
  ascending term, descending for a descending one); non-null columns get
  no extra key so indexes keep serving the order.
- One statement per page: `LIMIT n + 1`; the extra row only decides
  whether the next (forward) or previous (backward) cursor exists.
  `before` inverts every direction, fetches, then reverses the page.
- `select_related`/`prefetch_related` apply as in `all()` (reuse the
  existing fetch path; no per-row queries).
- Cursors in the result: forward page — `next` from the last item when
  the extra row existed, `previous` from the first item when the request
  had a cursor; backward page — `previous` from the first item when the
  extra row existed, `next` from the last item. An empty page has
  neither, except that an empty page reached with a cursor keeps that
  cursor's position as `previous` (forward) or `next` (backward) so the
  client can go back.
- Backends: SQL backends supported. MongoDB: supported when no ordering
  term is nullable; a nullable term is a capability error until the
  computed sort key is verified live. Redis: capability error. Add
  `Feature::KeysetPagination` and `Feature::NullableKeyset` to
  `capabilities.rs` so the checks are explicit and listed per backend.

### P4 — ORM API

- `QuerySet::after(token: &str)` / `before(token: &str)`: builders that
  remember the raw token (last call wins; using both is an error).
- `QuerySet::cursor_page(limit: u64) -> Result<CursorPage<M>, OrmError>`:
  the terminal. `limit` must be at least 1; decoding happens here
  because the fingerprint needs the final plan.
- `CursorPage<M> { items, next: Option<String>, previous:
  Option<String> }`, re-exported from the crate root next to `Page`.
- `OrmError::Cursor(CursorError)` (the enum is `non_exhaustive`).

### P5 — HTTP layer (`siderite-core::pagination`)

- `CursorParams<const MAX: u64 = 100, const DEFAULT: u64 = 20>` with
  `after`, `before` and `limit`, validated like `PageParams` (422 for an
  out-of-range limit or both cursors present; the query schema documents
  the bounds). `fetch(queryset)` returns `CursorPage<M>`.
- `siderite_core::CursorPage<T> { items, next, previous }` with `map`,
  `try_map`, `Dump` and `Schema` (`next`/`previous` nullable strings, no
  `total`).
- `ApiError: From<OrmError>` maps `OrmError::Cursor` to `400` with the
  error code as the problem `code` and a generic message.

### P6 — Tests and docs

- Turn the contract into executable tests: run every fixture case and
  token case against the implementation on SQLite in memory (default
  test run), and against PostgreSQL, MySQL and MongoDB as `#[ignore]`d
  live tests. Keep the reference-model test.
- Unit tests: each `CursorError` code, every tag round-trip, nullable
  terms in both directions, `before` pages, fingerprint changes with a
  filter value, rejected plans, capability errors on Redis and on MongoDB
  with a nullable term, `CursorParams` 422 cases and the 400 problem.
- Statement count: one fetch per page (assert with the existing query
  capture used by other tests, or a counting backend wrapper).
- Docs: `docs/ORM.md`, `website/.../guides/` pagination section, mark
  `CURSOR_PAGINATION.md` as implemented, `CHANGELOG.md`.

## F08 — CSRF and sessions

### S1 — Origin-checking CSRF middleware (`siderite-core`)

File: `crates/siderite-core/src/middleware/csrf.rs` (tests in
`csrf_tests.rs`), exported as `siderite_core::middleware::CsrfOrigin`.

- `CsrfOrigin::new(origins)` takes exact origins
  (`https://app.example.com`, scheme + host + optional port); invalid
  origins are a construction error (`Result`), normalized to lowercase
  with default ports removed.
- Safe methods (`GET`, `HEAD`, `OPTIONS`, `TRACE`) pass. For every other
  method: `Sec-Fetch-Site: same-origin` passes; otherwise `Origin` must
  equal an allowed origin; if `Origin` is absent, the origin of
  `Referer` is used; `Origin: null`, a missing pair, more than one
  header value or a foreign origin is `403` with one generic problem
  (no detail about which check failed). `Sec-Fetch-Site: cross-site`
  is rejected even when `Origin` would match.
- Never derived from `Host` or `X-Forwarded-*`.
- Optional `.exempt(path_prefix)` for webhooks, and
  `.only_with_cookie()` (default off) which skips requests that carry
  no `Cookie` header and no `Authorization: Basic`, for APIs mixing
  bearer tokens and cookies. Document the trade-off.

### S2 — Sessions crate (`crates/siderite-sessions`)

New workspace crate depending on `siderite-core` (adapter, as F08
decides; nothing in `siderite-core` depends on it). `getrandom` for ids.

- `SessionStore` trait (`async_trait`): `load(id_hash)`,
  `save(id_hash, record, ttl)`, `delete(id_hash)`, `revoke_user(user)`.
  Errors are a `StoreError` with no addresses in its message.
- `SessionRecord { user: Option<String>, data: serde_json::Map,
  created_at, last_seen, auth_epoch }`; ids are 32 random bytes,
  base64url, and stores only see the SHA-256 hex of the id.
- `SessionConfig`: cookie name (default `__Host-session`), `SameSite`
  (`Lax` default, `Strict`, `None` only together with the CSRF token
  check), idle timeout (default 30 min), absolute lifetime (default
  12 h), idle write interval (default a quarter of the idle timeout),
  store timeout (default 2 s), and `insecure_development()` which drops
  `Secure` and the `__Host-` prefix explicitly.
- `SessionLayer` middleware: loads the session by cookie, enforces idle
  and absolute expiry on the server, exposes it to handlers, writes back
  only when data changed or the idle write interval elapsed, and sets or
  expires the cookie. Store read failure on a request that carries a
  session cookie: `503` problem (fail closed). A failed write on login,
  rotation or change: `503` and no cookie.
- `Session` extractor: `get`, `insert`, `remove`, `user()`,
  `login(user)` (rotates: new id, old one deleted at once),
  `logout()` (deletes and expires the cookie), `rotate()`,
  `csrf_token()` (HMAC-SHA256 of the id hash with the configured key,
  base64url) and `verify_csrf(token)` in constant time.
- OpenAPI: a cookie `apiKey` security scheme named after the cookie,
  registered through the existing security scheme mechanism.
- `MemoryStore` for tests and single-process apps, with a bounded
  capacity and a `sweep()` for scheduled expiry removal.

### S3 — Cache-backed store

`CacheStore<C: siderite_cache::Cache>` behind the crate's `cache`
feature: records as JSON with the store TTL; `revoke_user` increments a
per-user epoch counter (`increment`), and `load` rejects records whose
`auth_epoch` is older than the user's current epoch, so revocation is
one write and works on MemoryCache and Redis. Live Redis tests are
`#[ignore]`d. An ORM table adapter stays in the follow-up backlog: the
schema is the application's choice.

### S4 — Tests and docs

- CSRF: every unsafe method, missing/foreign/`null` origins, `Referer`
  fallback, duplicate headers, `Sec-Fetch-Site` values, exemptions,
  `only_with_cookie`.
- Sessions: cookie flags (`__Host-`, `HttpOnly`, `Secure`, `Path=/`, no
  `Domain`, `SameSite`), id entropy/length, stores never see raw ids,
  login rotation deletes the old id, logout, revoke by user, idle and
  absolute expiry, idle write throttling, fail-closed reads and writes,
  store timeouts, CSRF token verification, redaction of the cookie.
- Docs: `docs/SECURITY.md`, `website/.../guides/http/security.md`,
  update `SESSIONS_CSRF.md` (S1-S3 implemented), `CHANGELOG.md`, and
  `deny.toml` if new dependencies need it.

## Order and verification

F02 and F08 are independent. Within each, P1 → P2 → P3 → P4 → P5 → P6
and S1 → S2 → S3 → S4. Before merge:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
python3 scripts/check_size.py
SIDERITE_UPDATE_DOCS_INDEX=1 cargo test -p siderite-cli docs
```
