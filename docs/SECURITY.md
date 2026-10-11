# Security

Security schemes are extractors that document themselves, in the spirit of FastAPI's `fastapi.security`. Reading credentials and describing them in OpenAPI happen in one place, so the generated document mirrors the handler signature. Items live in `siderite_core::security` and are re-exported as `siderite::security`; the commonly used types are also at the crate root.

The extractors **read** credentials. They never decide whether the credentials are valid. To do that, implement `Authenticate` and use `Security<T, S>` (see below).

## Schemes

| Extractor | Reads | OpenAPI scheme name | Missing or malformed | `WWW-Authenticate` |
|---|---|---|---|---|
| `HttpBearer` | `Authorization: Bearer <token>` | `HTTPBearer` | 401 | `Bearer` |
| `HttpBasic` | `Authorization: Basic <base64(user:pass)>` | `HTTPBasic` | 401 | `Basic realm="api"` |
| `ApiKey<S>` | header, query parameter or cookie named by `S` | `S::SCHEME` | 401 | none |
| `OAuth2PasswordBearer<S>` | `Authorization: Bearer <token>` | `S::SCHEME` | 401 | `Bearer` |
| `OAuth2PasswordRequestForm` | urlencoded token-endpoint body | not a scheme | 422 / 415 | none |

Details:

* The scheme name in `Authorization` is matched case-insensitively. A bearer token that is empty or contains whitespace is rejected as invalid.
* `HttpBasic` splits the decoded value at the **first** colon, so a password may contain colons. Bad base64, invalid UTF-8 or a missing colon is a 401.
* `ApiKey` takes the first non-empty value. A missing or empty key is a 401 without a challenge header, because API keys have no standard one.
* `OAuth2PasswordRequestForm` requires `username` and `password` (422 when absent or empty). `scope` is split on whitespace into `scopes`; `client_id` and `client_secret` are optional. `grant_type` is accepted and not checked. A content type other than `application/x-www-form-urlencoded` is a 415.
* `Debug` on every credential type prints `[REDACTED]` instead of tokens, keys, passwords and client secrets.

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

The application owns browser-session CSRF protection, secure HttpOnly cookie
policy where cookies are used, token storage, refresh rotation/revocation,
and issuer key refresh. Keep tokens out of URLs and logs. Browser storage
and cookies have different XSS/CSRF exposure; select and test one policy.
siderite does not ship sessions; the requirements for cookie flags, id
rotation, expiry, store failures, CSRF and trusted proxies that any
session layer should meet are in
[plans/SESSIONS_CSRF.md](plans/SESSIONS_CSRF.md).

Before returning a principal, a trusted verifier must check the access-token
signature with an allowed algorithm and keys from the configured issuer,
exact issuer, intended audience, expiration and not-before times with
bounded clock skew, token type and granted scopes. Opaque tokens require
trusted introspection or a server-side token store, rather than JWT parsing.
A decoded payload alone is never a verified token. Follow
[JWT BCP](https://www.rfc-editor.org/rfc/rfc8725.html) and the
[access-token profile](https://www.rfc-editor.org/rfc/rfc9068.html). Bound and cache trusted
key material; never fetch a token-supplied key URL.

## Verifying credentials: `Authenticate` and `Security`

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

`Security<T, S>` extracts `T::Credentials`, calls `T::authenticate(credentials, S::SCOPES, parts)` and documents the scheme together with the scopes. Scopes default to `NoScopes`, so `Security<CurrentUser>` requires none. The second tuple field is a type marker: destructure it as `_`.

| Outcome | Status |
|---|---|
| Credentials missing or malformed (the scheme extractor) | 401 with the scheme's challenge |
| `authenticate` rejects the credentials | whatever `ApiError` you return, normally 401 |
| `check_scopes` finds scopes missing | 403, `Missing required scopes: a, b.` |

`scopes!` declares a marker type implementing `Scopes` because const `&str` generics are not stable. It accepts a visibility and attributes (`scopes!(pub AdminScopes = ["users:read", "users:write"])`). `check_scopes(required, granted)` is a plain function you can call from any `Authenticate` implementation.

## OpenAPI output

Each scheme registers itself under `components.securitySchemes` and pushes a requirement onto the operation's `security`:

| Extractor | `securitySchemes` entry |
|---|---|
| `HttpBearer` | `{"type": "http", "scheme": "bearer"}` |
| `HttpBasic` | `{"type": "http", "scheme": "basic"}` |
| `ApiKey<S>` | `{"type": "apiKey", "in": "header" \| "query" \| "cookie", "name": S::NAME}` |
| `OAuth2PasswordBearer<S>` | `{"type": "oauth2", "flows": {"password": {"tokenUrl": .., "scopes": {..}}}}` |

`OAuth2PasswordRequestForm` documents an `application/x-www-form-urlencoded` request body with `username` and `password` required. `Security<T, S>` re-emits the scheme its credentials add, listing `S::SCOPES` as the scopes on that same requirement object. Duplicate scheme names on one operation collapse to a single entry.

## Constant-time comparison

The extractors return raw values and never compare them, so timing is your code's concern. Compare API keys, tokens and passwords in constant time (for example with the `subtle` crate), not with `==`, which returns early at the first differing byte. Store password hashes, not passwords, and compare hashes with a purpose-built verifier.

## Requirements in OpenAPI

Security requirement objects are alternatives, and the schemes inside one
object are all required. Extractors follow the handler signature:

| Handler arguments | `security` |
|---|---|
| `HttpBearer` | `[{"HTTPBearer": []}]` |
| `HttpBearer, ApiKey<K>` | `[{"HTTPBearer": [], "K": []}]` (both required) |
| `Option<HttpBearer>` | `[{}, {"HTTPBearer": []}]` (anonymous or bearer) |
| `ApiKey<K>, Option<HttpBearer>` | `[{"K": []}, {"K": [], "HTTPBearer": []}]` |

At runtime, `Option<Scheme>` is `None` only when no credentials are sent.
Malformed credentials, or a `Security<T, S>` whose `authenticate` rejects
them, still return `401`.

## Legacy password-flow compatibility

`OAuth2Spec`, `OAuth2PasswordBearer` and `OAuth2PasswordRequestForm` remain
available for legacy contracts and describe a password flow in OpenAPI.
They are not the recommended authorization architecture. The form accepts
`grant_type` without verifying it; applications retaining legacy endpoints
own grant validation and token issuance. The blog example demonstrates this
compatibility surface and is not a modern authorization-server template.
