---
title: Security
description: Bearer validation, authorization code with PKCE, security schemes, sessions and CSRF.
---

Security schemes are extractors that document themselves. Reading credentials
and describing them in OpenAPI happen in one place, so the generated
document mirrors the handler signature. Items live in
`siderite_core::security` and are re-exported as `siderite::security`.

The extractors **read** credentials. They never decide whether the
credentials are valid. Implement `Authenticate` and use `Security<T, S>`
for that.

## Schemes

| Extractor | Reads | OpenAPI scheme name | Missing or malformed | `WWW-Authenticate` |
|---|---|---|---|---|
| `HttpBearer` | `Authorization: Bearer <token>` | `HTTPBearer` | 401 | `Bearer` |
| `HttpBasic` | `Authorization: Basic <base64(user:pass)>` | `HTTPBasic` | 401 | `Basic realm="api"` |
| `ApiKey<S>` | header, query parameter, or cookie named by `S` | `S::SCHEME` | 401 | none |
| `OAuth2PasswordBearer<S>` | `Authorization: Bearer <token>` | `S::SCHEME` | 401 | `Bearer` |
| `OAuth2PasswordRequestForm` | urlencoded token-endpoint body | not a scheme | 422 / 415 | none |

- The scheme name in `Authorization` is matched case-insensitively. A
  bearer token that is empty or contains whitespace is rejected.
- `HttpBasic` splits the decoded value at the **first** colon, so a
  password may contain colons. Bad base64, invalid UTF-8, or a missing
  colon is a 401.
- `ApiKey` takes the first non-empty value. A missing or empty key is a
  401 without a challenge header.
- `OAuth2PasswordRequestForm` requires `username` and `password` (422 when
  absent or empty). `scope` is split on whitespace into `scopes`;
  `client_id` and `client_secret` are optional. `grant_type` is accepted
  and not checked. A content type other than
  `application/x-www-form-urlencoded` is a 415.
- `Debug` on every credential type prints `[REDACTED]`.

```rust
use siderite::security::{ApiKey, ApiKeyLocation, ApiKeySpec};

struct PartnerKey;

impl ApiKeySpec for PartnerKey {
    const NAME: &'static str = "X-API-Key";
    const LOCATION: ApiKeyLocation = ApiKeyLocation::Header; // or Query, Cookie
    const SCHEME: &'static str = "PartnerKey";
}

async fn feed(key: ApiKey<PartnerKey>) -> String {
    format!("key of {} bytes", key.key.len())
}
```

## Recommended OAuth flow

Use authorization code with PKCE (S256) through an authorization server.
The password grant is legacy compatibility only; RFC 9700 section 2.4
prohibits it for current deployments. Siderite extracts API credentials;
it does not implement the authorization server or a JWT verifier.
See [OAuth security BCP](https://www.rfc-editor.org/rfc/rfc9700.html#section-2.4).

1. Create a fresh high-entropy PKCE verifier and one-time state bound to the
   browser session. Redirect to the configured issuer's authorization
   endpoint with an exact registered redirect URI, `response_type=code`,
   `code_challenge_method=S256` and the challenge. For OpenID Connect use
   a fresh nonce and verify it on the returned ID token.
2. On callback, verify the bound state/issuer and exchange the code once at
   the configured token endpoint with the original verifier and redirect
   URI. Do not accept arbitrary issuer, redirect or token-endpoint URLs
   from request input. Require TLS and provider-supported PKCE.
3. Send the access token to the API in `Authorization: Bearer ...`.
   `HttpBearer` plus `Security<T, S>` works independently of the OAuth grant.
   An ID token intended for a client is not an API access token.

Protocol details are in [PKCE](https://www.rfc-editor.org/rfc/rfc7636.html).

Browser sessions and CSRF protection are covered in
[Browser sessions and CSRF](#browser-sessions-and-csrf). The application
owns token storage, refresh rotation/revocation, and issuer key refresh. Keep tokens out of URLs and logs. Browser storage
and cookies have different XSS/CSRF exposure; select and test one policy.

Before returning a principal, a trusted verifier must check the access-token
signature with an allowed algorithm and keys from the configured issuer,
exact issuer, intended audience, expiration and not-before times with
bounded clock skew, token type and granted scopes. Opaque tokens require
trusted introspection or a server-side token store, rather than JWT parsing.
A decoded payload alone is never a verified token. Follow
[JWT BCP](https://www.rfc-editor.org/rfc/rfc8725.html) and the
[access-token profile](https://www.rfc-editor.org/rfc/rfc9068.html). Bound and cache trusted
key material; never fetch a token-supplied key URL.

## `Authenticate` and `Security`

```rust
use siderite::security::{Authenticate, HttpBearer, Security, check_scopes};
use siderite::{ApiError, scopes};
use http::request::Parts;
use std::sync::Arc;

struct VerifiedAccess {
    subject: String,
    scopes: Vec<String>,
}

// Application interface: supply a trusted JWT verifier or introspection
// implementation. There is deliberately no permissive default verifier.
trait AccessVerifier: Send + Sync {
    // Verify signature, allowed algorithm, issuer, audience, expiry,
    // not-before and token type before returning these trusted fields.
    fn verify(&self, token: &str) -> Result<VerifiedAccess, ApiError>;
}

struct CurrentUser(String);

impl Authenticate for CurrentUser {
    type Credentials = HttpBearer;

    async fn authenticate(
        credentials: HttpBearer,
        required: &[&'static str],
        parts: &Parts,
    ) -> Result<Self, ApiError> {
        let verifier = parts.extensions
            .get::<Arc<dyn AccessVerifier>>()
            .ok_or_else(|| ApiError::internal("Verifier not configured."))?;
        let access = verifier.verify(&credentials.token)?;
        check_scopes(required, access.scopes.iter().map(String::as_str))?;
        Ok(CurrentUser(access.subject))
    }
}

scopes!(pub ReadScopes = ["read"]);

async fn me(Security(user, _): Security<CurrentUser, ReadScopes>) -> String {
    user.0
}
```

Install the verifier with `App::with_state(Arc<dyn AccessVerifier>)`.
The interface above shows the ownership boundary; implement it with a
verified token library/provider, not handwritten JWT cryptography.
Integration tests must reject a forged signature, wrong issuer/audience,
expired or not-yet-valid access token, ID-token substitution, missing scope
and untrusted key ID/URL. Test state/nonce replay and redirect mismatch at
the OAuth callback too. These provider checks are application tests;
Siderite's extraction/scopes tests cannot establish their correctness.

`Security<T, S>` extracts `T::Credentials`, calls
`T::authenticate(credentials, S::SCOPES, parts)`, and documents the scheme
together with the scopes. Scopes default to `NoScopes`, so
`Security<CurrentUser>` requires none. The second tuple field is a type
marker: destructure it as `_`.

| Outcome | Status |
|---|---|
| Credentials missing or malformed | 401 with the scheme’s challenge |
| `authenticate` rejects the credentials | whatever `ApiError` you return, normally 401 |
| `check_scopes` finds scopes missing | 403, `Missing required scopes: a, b.` |

`scopes!` declares a marker type implementing `Scopes` because const `&str`
generics are not stable. `check_scopes(required, granted)` is a plain
function you can call from any `Authenticate` implementation.

## OpenAPI output

Each scheme registers itself under `components.securitySchemes` and pushes
a requirement onto the operation’s `security`.

| Extractor | `securitySchemes` entry |
|---|---|
| `HttpBearer` | `{"type": "http", "scheme": "bearer"}` |
| `HttpBasic` | `{"type": "http", "scheme": "basic"}` |
| `ApiKey<S>` | `{"type": "apiKey", "in": "header" \| "query" \| "cookie", "name": S::NAME}` |
| `OAuth2PasswordBearer<S>` | `{"type": "oauth2", "flows": {"password": {"tokenUrl": .., "scopes": {..}}}}` |

Security requirement objects are alternatives; the schemes inside one
object are all required:

| Handler arguments | `security` |
|---|---|
| `HttpBearer` | `[{"HTTPBearer": []}]` |
| `HttpBearer, ApiKey<K>` | `[{"HTTPBearer": [], "K": []}]` (both required) |
| `Option<HttpBearer>` | `[{}, {"HTTPBearer": []}]` (anonymous or bearer) |
| `ApiKey<K>, Option<HttpBearer>` | `[{"K": []}, {"K": [], "HTTPBearer": []}]` |

At runtime, `Option<Scheme>` is `None` only when no credentials are sent.
Malformed credentials, or a `Security<T, S>` whose `authenticate` rejects
them, still return `401`; an invalid token never falls back to anonymous.

`Security<T, S>` re-emits the scheme its credentials add, listing
`S::SCOPES` on that same requirement object. Duplicate scheme names on one
operation collapse to a single entry.

## Constant-time comparison

The extractors return raw values and never compare them. Compare API keys,
tokens, and passwords in constant time (for example with the `subtle`
crate), not with `==`. Store password hashes, not passwords, and compare
hashes with a purpose-built verifier.

`RouteCache` stores only responses that opt in (`Cache-Control: public`,
or a per-route layer with `default_ttl`), so responses that depend on who
is asking stay out of the cache unless you mark them. Requests with
credential-like headers (`Authorization`, `Cookie`, `X-API-Key`, and names
containing `auth`, `token`, `session`, `jwt`, `secret`, `api-key` or
`access-key`) skip the cache as well. See
[Cache](/siderite/guides/production/cache/).

## Browser sessions and CSRF

CSRF matters wherever the browser sends credentials by itself: cookies and
HTTP Basic. Bearer tokens and API keys that script code puts in a header are
not ambient and need no CSRF defence. Safe methods (`GET`, `HEAD`,
`OPTIONS`, `TRACE`) must never change state.

Two pieces cover cookie-authenticated apps. `CsrfOrigin` (in
`siderite-core`) works for any cookie, whoever issued it. `siderite-sessions`
is an adapter that owns cookie mechanics, session ids, rotation, expiry and
CSRF tokens; users, login flows and password storage stay in your
application, and nothing in `siderite-core` depends on it.

### `CsrfOrigin`

```rust
use siderite::middleware::CsrfOrigin;

let csrf = CsrfOrigin::new(["https://app.example.com"])?
    .exempt("/webhooks/");           // signed webhooks, no cookies
let app = App::new().csrf_origin(csrf); // register before SessionLayer
```

For every method other than `GET`, `HEAD`, `OPTIONS` and `TRACE`:

1. `Sec-Fetch-Site: cross-site` is rejected, even when `Origin` matches;
   `Sec-Fetch-Site: same-origin` is accepted.
2. Otherwise `Origin` must equal a configured origin; when `Origin` is
   absent the origin of `Referer` is used.
3. `Origin: null`, a missing `Origin` and `Referer`, repeated headers and a
   foreign origin are all rejected.

A rejection is one generic `403` problem that does not say which check
failed. Origins are `scheme://host[:port]`, lowercased with default ports
removed; an invalid origin is a construction error. The allowed origins are
configuration only: they are never derived from `Host` or `X-Forwarded-*`,
so a forged header cannot widen them.

`exempt(prefix)` skips whole path segments (`/hooks` does not cover
`/hooksy`) and never matches paths with `.` or `..` segments; exempt routes
must authenticate callers another way, such as a webhook signature.
`only_with_cookie()` (off by default) skips requests that carry no `Cookie`
header and no `Authorization: Basic`, for APIs that mix bearer tokens and
cookies. The trade-off: such a request reaches the handler unchecked, so the
route must reject anonymous state changes itself, and any other credential a
browser attaches by itself would bypass the check.

### Sessions

```rust
use siderite_sessions::{MemoryStore, Session, SessionConfig, SessionLayer};

async fn login(session: Session /* , your own credential check */) {
    session.login("ada"); // rotates the id; the old id is deleted at once
}

let layer = SessionLayer::new(MemoryStore::default(), SessionConfig::new())?;
let app = App::new().route("/login", post(login)).layer(layer);
```

`Session` is an extractor with `get`, `insert`, `remove`, `user`, `login`,
`logout`, `rotate`, `csrf_token` and `verify_csrf`. A request without a
cookie gets no stored session until it stores something, logs in, rotates or
asks for a CSRF token.

| Setting | Default |
|---|---|
| Cookie | `__Host-session`: `HttpOnly`, `Secure`, `Path=/`, no `Domain` |
| `SameSite` | `Lax`; `Strict` as an option; `None` only with `Secure` and a CSRF key |
| Idle timeout | 30 minutes |
| Absolute lifetime | 12 hours (login restarts it, rotation does not) |
| Idle write interval | a quarter of the idle timeout |
| Store timeout | 2 seconds per call |
| `insecure_development()` | drops `Secure` and the `__Host-` prefix; always explicit, never inferred from the request |

What the layer guarantees:

- **Ids** are 32 bytes (256 bits) from the operating system's CSPRNG,
  base64url. The cookie holds only the id. Stores see only its SHA-256 hex
  (`id_hash`), so a leaked table or backup hands out no live sessions; ids
  are never logged and the `Cookie` header is never recorded.
- **Rotation:** `login` and `rotate` issue a new id and delete the old one
  with no grace period. `logout` deletes the session and expires the cookie.
  `SessionStore::revoke_user` ends every session of a user, for password
  changes and account recovery.
- **Expiry** is enforced on the server from the stored record; cookie expiry
  is advisory. The idle timestamp is written at most once per write interval,
  so reads do not become writes (a session can expire up to one interval
  early, never late). Remove expired rows with a scheduled job
  (`MemoryStore::sweep`, or the cache's TTL), not on the request path.
- **Fail closed:** if the store cannot be read (error or timeout) for a
  request that carries a session cookie, the answer is a `503` problem, never
  an anonymous fallback. A failed write for a login, rotation or change is a
  `503` and no cookie is set for a session that was not stored. Every store
  call is bounded by the store timeout. Problems never name the store.
- **CSRF tokens:** `Session::csrf_token()` is the HMAC-SHA256 of the session
  id hash under `SessionConfig::csrf_key` (at least 32 bytes), base64url; it
  changes when the id rotates. `verify_csrf` compares in constant time. With
  `SameSite::None` the layer itself requires the token, in the `x-csrf-token`
  header, on unsafe requests that carry a live session. With the default
  `Lax`, pair the layer with `CsrfOrigin`, and add tokens for forms.

Stores implement `SessionStore` (`load`, `save`, `delete`, `revoke_user`, and
an `auth_epoch` hook). `MemoryStore` is bounded (a full store refuses new
sessions with `503` rather than evicting a login) and lives in one process.
`CacheStore` (feature `cache`) keeps JSON records with the store TTL in any
`siderite_cache::Cache`, Redis included; revocation increments a per-user
epoch counter and `load` rejects older records, so it is one write. Use a
cache that does not evict those counters (Redis `noeviction` or a
`volatile-*` policy; a small `MemoryCache` is an LRU). Cache error texts,
which can contain addresses, never reach logs or clients. An ORM table
adapter is not provided: the schema is the application's choice.

Limits to know: concurrent requests of one session are last-writer-wins;
the OpenAPI document gets a `sessionCookie` cookie `apiKey` scheme that
names the default cookie, as an optional requirement; binding sessions to
client IP addresses is not recommended, and if you do it the address must
come from `TrustedProxies`, never from raw forwarding headers.

## See also

- [OpenAPI 3.1](/siderite/guides/http/openapi/)
- [Errors](/siderite/guides/http/errors/)
- [Blog on PostgreSQL](/siderite/tutorials/blog-postgres/) — OAuth2 password flow in an example

## Legacy password-flow compatibility

`OAuth2Spec`, `OAuth2PasswordBearer` and `OAuth2PasswordRequestForm` remain
available for legacy contracts and describe a password flow in OpenAPI.
They are not the recommended authorization architecture. The form accepts
`grant_type` without verifying it; applications retaining legacy endpoints
own grant validation and token issuance. The blog example demonstrates this
compatibility surface and is not a modern authorization-server template.
