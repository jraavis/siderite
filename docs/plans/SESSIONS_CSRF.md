# Sessions and CSRF: decision (F08)

Status: decided for now, open to review. Nothing here is implemented, and
this document promises no session implementation. It fixes what siderite
does and does not own, and the requirements any later session or CSRF
work must meet.

## Decision

siderite does **not** ship a session store or session middleware now.
Applications that need browser sessions either:

1. keep using token authentication (`Authorization: Bearer`, API keys),
   which siderite already extracts and documents, or
2. add an established session layer with `App::layer` (for example
   `tower-sessions` with a store for their database), configured as
   below.

If demand grows, the next step is an **adapter**, not an owned stack: a
`Session` extractor and an OpenAPI cookie security scheme over a small
store trait, with stores supplied by adapters (ORM, Redis through
`siderite-cache`). siderite would own cookie mechanics, rotation hooks
and CSRF checks; never identity, login flows, password storage or the
store's data model.

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

## Follow-up backlog (not promised)

- S1: Origin-checking CSRF middleware with configured origins, tested for
  every unsafe method, missing and foreign origins and `Sec-Fetch-Site`.
- S2: `Session` extractor and OpenAPI cookie scheme over a store trait,
  with the requirements above as executable tests.
- S3: store adapters (ORM table, Redis) with expiry, revocation by user
  and failure behaviour tested against live services.
