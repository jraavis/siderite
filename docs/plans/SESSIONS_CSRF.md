# Sessions and CSRF: decision (F08)

Status: S1-S3 implemented (see the backlog at the end); S4 tests and docs
are part of that work. This document fixes what siderite does and does not
own, and the requirements session and CSRF work must meet. User-facing
documentation is in [SECURITY.md](../SECURITY.md); the implementation is
`CsrfOrigin` in `siderite-core` and the `siderite-sessions` crate.

## Decision

siderite owns an **adapter**, not an identity stack: the `siderite-sessions`
crate (a `Session` extractor, a layer and a store trait, with `MemoryStore`
and a cache-backed store) and the `CsrfOrigin` middleware. Applications that
prefer another session layer can still:

1. keep using token authentication (`Authorization: Bearer`, API keys),
   which siderite already extracts and documents, or
2. add an established session layer with `App::layer` (for example
   `tower-sessions` with a store for their database), configured as
   below.

The adapter is a `Session` extractor and an OpenAPI cookie security scheme
over a small store trait, with stores supplied by adapters (Redis through
`siderite-cache`; an ORM table is left to the application, whose schema it
is). siderite owns cookie mechanics, rotation hooks and CSRF checks; never
identity, login flows, password storage or the store's data model.

Why not an owned implementation: session ids, rotation, expiry
sweeping, store consistency and CSRF are security-critical code with a
long review tail, and well-tested layers exist. An adapter keeps one
place (the store trait) where siderite's error model, redaction and
OpenAPI description meet someone else's storage.

The one piece worth building first, independent of sessions, is an
Origin-checking CSRF middleware (below): it protects any app that
authenticates with cookies, whoever issues them.

## Requirements for any session support

### Cookie

- Name with the `__Host-` prefix by default: the browser then requires
  `Secure`, `Path=/` and no `Domain`, so subdomains cannot set or read it.
- `HttpOnly`, `Secure`, `Path=/`, no `Domain`.
- `SameSite=Lax` by default; `Strict` as an option. `SameSite=None` only
  together with `Secure` and the CSRF token defence.
- Plain-HTTP development must be an explicit opt-in that drops `Secure`
  and the prefix; it is never inferred from the request.
- The cookie holds only an opaque id: no user data, no signed state.

### Session ids

- At least 128 bits from the operating system's CSPRNG, URL-safe
  encoded.
- The store keeps a hash of the id (SHA-256), so a leaked table does not
  hand out live sessions; ids are never logged (redaction covers the
  `Cookie` header already).

### Rotation and revocation

- A new id on login, on any privilege change and on re-authentication;
  the old id is deleted at once (no grace period).
- Logout deletes the server-side session and expires the cookie.
- Revoking every session of a user is a store operation (an index by
  user), used after password changes and account recovery.

### Expiry

- Both an idle timeout (for example 30 minutes) and an absolute lifetime
  (for example 12 hours), enforced on the server; cookie expiry is only
  advisory.
- Idle extension writes at most once per fraction of the idle period
  (not on every request), so reads do not become writes.
- Expired rows are removed by a scheduled job, not on the request path.

### Store failures

- Fail closed: if the store cannot be read, requests that need a
  session get `503` (a redacted problem), never an anonymous fallback
  that silently drops authentication.
- A failed write on login or rotation fails the request with `503`; no
  cookie is set for a session that was not stored.
- Store calls are bounded by timeouts; a readiness check (F04) can report
  the store, without its address.

## CSRF strategy

CSRF matters wherever the browser sends credentials by itself (cookies,
HTTP authentication). Bearer tokens and API keys sent in headers by
script are not ambient and need no CSRF defence.

1. **Safe methods are safe.** `GET`, `HEAD` and `OPTIONS` never change
   state; generators and examples follow this.
2. **Origin check (default).** For unsafe methods on cookie-authenticated
   routes, require `Origin` (or, if absent, `Referer`) to match one of the
   configured public origins exactly; `Sec-Fetch-Site: same-origin` is
   accepted as a positive signal where present. A missing or foreign
   origin is `403`. The allowed origins are configuration, never derived
   from the request's own `Host`.
3. **Token (for forms and non-browser clients).** A synchronizer token
   stored in the session, or a double-submit token that is an HMAC of the
   session id with a server key, sent in a header or form field and
   compared in constant time. Required when `SameSite=None` is used.
4. Failures are `403` problems without detail about which check failed.

## Trusted proxies

- Scheme and host come from `X-Forwarded-Proto` / `X-Forwarded-Host` only
  when the immediate peer is in `TrustedProxies`; otherwise from the
  connection. `Secure` cookies and HTTPS redirects rely on this.
- Origin checks compare with configured origins, so a forged
  `X-Forwarded-Host` cannot widen them.
- Binding sessions to client IP addresses is not recommended (NAT, mobile
  networks); if an application does it, the address must come from
  `TrustedProxies`, never from raw forwarding headers.

## Ownership

| Concern | Application | siderite (if the adapter is built) |
|---|---|---|
| Users, login, password hashing, lockout | yes | no |
| Store choice and schema | yes | trait and adapters |
| Cookie flags, id generation, rotation, expiry checks | configures | implements |
| CSRF origin check and tokens | configures origins | implements |
| Trusted proxy list | configures | applies |
| Readiness of the store | registers the check | runs it (F04) |

## Backlog

- S1 (implemented): `siderite_core::middleware::CsrfOrigin`, tested for
  every unsafe method, missing, foreign and `null` origins, `Referer`
  fallback, duplicate headers, `Sec-Fetch-Site`, exemptions and
  `only_with_cookie`.
- S2 (implemented): `siderite-sessions` with `Session`, `SessionLayer`,
  `SessionConfig`, `SessionStore`, `MemoryStore` and an OpenAPI cookie
  scheme, with the requirements above as executable tests: cookie flags,
  id length and hashing, rotation, logout, revocation, idle and absolute
  expiry, idle write throttling, fail-closed reads and writes, store
  timeouts, CSRF tokens and cookie redaction.
- S3 (implemented): `CacheStore` (feature `cache`) over any
  `siderite_cache::Cache`, with TTL expiry, revocation by user through an
  epoch counter and failure behaviour. Live Redis tests are `#[ignore]`d
  (`REDIS_URL`, database 15). An ORM table adapter is not provided.

Decisions made while implementing:

- `SessionStore` has a fifth method, `auth_epoch(user)` (default `0`),
  which login reads so the record carries the user's revocation epoch;
  that is what stops a write racing `revoke_user` from reviving a session.
- `SameSite=None` is accepted only with `Secure` and a CSRF key, and the
  layer then checks the token header on unsafe requests that carry a live
  session, so the "token defence" cannot be forgotten.
- The OpenAPI scheme (`sessionCookie`) names the default cookie and is an
  optional requirement; a custom cookie name is not reflected in it.
- The facade crate does not re-export `siderite-sessions`; depend on it
  directly.
- Existing sessions are written with `SessionStore::update`, which only
  replaces a stored record, so a slow request racing a logout or a
  rotation cannot store the deleted id again. `CacheStore` has no
  conditional write, so its `delete` also leaves a tombstone (24 hours
  by default) that `load` checks.
