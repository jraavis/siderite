//! [`RouteCache`]: HTTP GET/HEAD response cache as an `App` layer.

use crate::{Cache, CacheError};
use http::header::PROXY_AUTHORIZATION;
use http::header::{ACCEPT, ACCEPT_ENCODING, AUTHORIZATION};
use http::header::{CACHE_CONTROL, COOKIE};
use http::header::{HOST, SET_COOKIE, VARY};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use siderite_core::middleware::TrustedProxies;
use siderite_core::middleware::{BoxService, Next, from_fn};
use siderite_core::{ApiError, Body, IntoResponse, Request, Response};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tower::Layer;

mod codec;
mod generation;
use codec::{decode, encode};
use generation::representation_key as make_key;
type CacheResult<T> = Result<T, CacheError>;
type Directive = (String, Option<String>);
type Hit = Option<(HeaderMap, Vec<u8>)>;

const X_CACHE: HeaderName = HeaderName::from_static("x-cache");
const HIT: HeaderValue = HeaderValue::from_static("hit");
const MISS: HeaderValue = HeaderValue::from_static("miss");

/// Largest body stored by default (1 MiB).
pub const DEFAULT_MAX_BODY_BYTES: u64 = 1024 * 1024;

/// Default lifetime of a target's generation marker (one hour).
pub const DEFAULT_GENERATION_TTL: Duration = Duration::from_secs(3600);

/// Default admission budget: new markers per namespace per
/// `generation_ttl` window.
pub const DEFAULT_MAX_NEW_TARGETS: u64 = 10_000;

/// Shared flag: a failed invalidation disabled the layer.
const DISABLED: u8 = 1;
/// Shared flag: no expiring generation markers (unsupported backend or a
/// zero `generation_ttl`), so the layer neither stores nor invalidates.
const NO_MARKERS: u8 = 2;

/// Request headers that are part of the cache key; a response may `Vary` on
/// these and still be stored.
const KEYED_HEADERS: [HeaderName; 2] = [ACCEPT, ACCEPT_ENCODING];

/// Hop-by-hop headers (RFC 9110) plus the marker this layer writes.
fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
            | "x-cache"
    )
}

/// Middleware that caches successful GET and HEAD responses.
///
/// Caching is **opt-in**. There are two ways to opt a route in:
///
/// * **Per response.** Put the layer on the whole app; it stores only
///   responses marked `Cache-Control: public` (for example with
///   [`Cached::public`](siderite_core::Cached)). Everything else passes
///   through.
/// * **Per route.** Put the layer on one route with `MethodRouter::layer` and
///   set [`default_ttl`](RouteCache::default_ttl); that route's responses are
///   stored without a `public` marker.
///
/// ```
/// use siderite_cache::{MemoryCache, RouteCache};
/// use siderite_core::{App, Cached, get};
/// use std::time::Duration;
///
/// let cache = MemoryCache::new(1024);
/// let _app = App::new()
///     // Opted in by its response.
///     .route("/news", get(|| async {
///         Cached::public(Duration::from_secs(60), "news")
///     }))
///     // Opted in by placement.
///     .route("/stats", get(|| async { "stats" }).layer(
///         RouteCache::new(cache.clone())
///             .default_ttl(Duration::from_secs(30)),
///     ))
///     // Never stored: no `public` marker.
///     .route("/me", get(|| async { "private" }))
///     .layer(RouteCache::new(cache));
/// ```
///
/// Do not set `default_ttl` on an app-wide layer unless every GET route in
/// the app is safe to share between users.
///
/// # Behaviour
/// * Only GET and HEAD responses are stored. Successful unsafe target
///   writes rotate a shared generation across every representation.
///   Related URI dependencies require explicit application invalidation.
/// * The cache key is the method, scheme, `Host`, path and query (as
///   received), plus the request's `Accept` and `Accept-Encoding` values, so
///   virtual hosts sharing one cache never see each other's responses.
/// * Requests carrying credentials bypass the cache: `Authorization`,
///   `Proxy-Authorization`, `Cookie`, `X-API-Key`, any header whose name
///   contains `auth`, `token`, `session`, `jwt`, `secret`, `api-key`,
///   `apikey` or `access-key`, and any header added with
///   [`RouteCache::bypass_header`]. Such requests are neither
///   answered from nor stored in the cache.
/// * Responses are stored only when the status is 200, there is no
///   `Set-Cookie`, `Cache-Control` does not contain `no-store`, `no-cache` or
///   `private`, `Vary` names no header outside the key, and the body length
///   is known and at most [`RouteCache::max_body_bytes`]. Streaming bodies
///   (server-sent events, file streams) pass through untouched.
/// * On top of that, a response needs `Cache-Control: public` unless
///   [`default_ttl`](RouteCache::default_ttl) is set.
/// * The entry lives for `s-maxage`, else `max-age`, else `default_ttl`. A
///   `public` response without any of these, or with a lifetime of zero, is
///   not stored.
/// * Request no-cache refreshes; no-store bypasses without deleting older
///   entries. Unsupported conditionals and ranges bypass. Date is retained
///   and Age includes origin age and residence. Entries are versioned.
/// * Namespaces isolate layers by default. Stable namespaces intentionally
///   share generations; key, header and encoded byte admission is bounded.
/// * Every GET/HEAD response is tagged with `x-cache: hit` or `x-cache: miss`.
///
/// Cache backend failures fail open: the request is served and treated as a
/// miss. Failed write invalidation disables this layer. A database commit
/// and distributed cache invalidation are separate operations; other nodes
/// need an outage/restart consistency policy. No secret values are logged.
#[derive(Clone, Debug)]
pub struct RouteCache<C> {
    cache: Arc<C>,
    default_ttl: Option<Duration>,
    max_body: u64,
    bypass: Arc<Vec<HeaderName>>,
    namespace: Arc<str>,
    proxies: TrustedProxies,
    flags: Arc<AtomicU8>,
    max_key: usize,
    max_headers: usize,
    max_encoded: usize,
    generation_ttl: Duration,
    max_new_targets: u64,
}

impl<C: Cache> RouteCache<C> {
    /// Cache GET/HEAD 200 responses marked `Cache-Control: public` in
    /// `cache`.
    #[must_use]
    pub fn new(cache: C) -> Self {
        Self {
            cache: Arc::new(cache),
            default_ttl: None,
            max_body: DEFAULT_MAX_BODY_BYTES,
            namespace: uuid::Uuid::new_v4().to_string().into(),
            proxies: TrustedProxies::default(),
            flags: Arc::new(AtomicU8::new(0)),
            max_key: 4096,
            max_headers: 8192,
            max_encoded: 2 * 1024 * 1024,
            generation_ttl: DEFAULT_GENERATION_TTL,
            max_new_targets: DEFAULT_MAX_NEW_TARGETS,
            bypass: Arc::new(vec![
                AUTHORIZATION,
                PROXY_AUTHORIZATION,
                COOKIE,
                HeaderName::from_static("x-api-key"),
            ]),
        }
    }

    /// Set a stable app/deployment namespace for intentionally shared caches.
    ///
    /// Args:
    ///     namespace: Identity distinct from incompatible apps or revisions.
    ///
    /// Returns:
    ///     Configured layer; the default namespace is unique per instance.
    #[must_use]
    pub fn namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into().into();
        self
    }

    /// Resolve cache request scheme with the shared trusted-peer policy.
    ///
    /// Args:
    ///     proxies: Same immediate-peer policy used by other middleware.
    ///
    /// Returns:
    ///     Layer ignoring forwarded headers from untrusted peers.
    #[must_use]
    pub fn trusted_proxies(mut self, proxies: TrustedProxies) -> Self {
        self.proxies = proxies;
        self
    }

    /// Bound keys, stored header bytes and serialized response bytes.
    ///
    /// Args:
    ///     key: Maximum key bytes, including namespace and generation.
    ///     hdrs: Maximum stored header-name/value bytes.
    ///     encoded: Maximum versioned serialized response size.
    ///
    /// Returns:
    ///     Layer bypassing oversized entries without failing the origin.
    #[must_use]
    pub fn byte_limits(self, key: usize, hdrs: usize, encoded: usize) -> Self {
        let mut settings = self;
        settings.max_key = key;
        settings.max_headers = hdrs;
        settings.max_encoded = encoded;
        settings
    }

    /// Set the lifetime of per-target generation markers.
    ///
    /// Markers are created with this expiry (default
    /// [`DEFAULT_GENERATION_TTL`], one hour) and stored representations
    /// never outlive it: a representation's TTL is clamped to
    /// `min(response ttl, generation_ttl)`. When a marker expires, the next
    /// request draws a fresh random generation, so earlier or in-flight
    /// fills become unreachable and read as safe misses.
    ///
    /// Args:
    ///     ttl: Marker lifetime. Zero means route caching is disabled:
    ///         no request touches the backend, nothing is stored or
    ///         invalidated, and responses carry `x-cache: miss`. A value
    ///         above the backend's portable limit is rejected by the
    ///         backend, so every request bypasses the cache.
    ///
    /// Note: `max-age`/`s-maxage` above `ttl` is capped to `ttl` (default
    /// one hour), as is `default_ttl`.
    ///
    /// Returns:
    ///     Configured layer. A custom [`Cache`] without
    ///     `set_if_absent_with_ttl` makes the layer bypass storage.
    #[must_use]
    pub fn generation_ttl(mut self, ttl: Duration) -> Self {
        self.generation_ttl = ttl;
        if ttl.is_zero() {
            self.flags.fetch_or(NO_MARKERS, Ordering::Release);
        } else {
            self.flags.fetch_and(!NO_MARKERS, Ordering::Release);
        }
        self
    }

    /// Bound the metadata one namespace can create (default
    /// [`DEFAULT_MAX_NEW_TARGETS`]).
    ///
    /// At most `max` new target markers are admitted per namespace in each
    /// `generation_ttl` window, counted when a representation is first
    /// stored for a target. Once the window's budget is spent, further new
    /// targets bypass the cache (`x-cache: miss`) until the next window;
    /// targets that already have a marker keep hitting. Because markers
    /// live one `generation_ttl`, a namespace holds at most `2 * max`
    /// markers. Zero stores nothing and makes no backend calls.
    ///
    /// The counter lives in the backend, so the bound holds across
    /// processes sharing a namespace, provided the backend does not evict
    /// the counter. A [`Cache`] without `increment_with_expiry` cannot
    /// enforce it, so the layer stops storing (one warning).
    #[must_use]
    pub fn max_new_targets(mut self, max: u64) -> Self {
        self.max_new_targets = max;
        self
    }

    /// Invalidate every cached representation of one related target URI.
    ///
    /// Args:
    ///     req: Request identifying the target authority, scheme and URI.
    ///
    /// Returns:
    ///     Success after rotating its shared backend generation. Use this
    ///     for application-specific list/detail dependencies after writes.
    ///
    /// # Errors
    /// Invalid context, oversized key or cache backend failure. A failed
    /// invalidation disables this layer to avoid stale local reuse.
    pub async fn invalidate_target(&self, req: &Request) -> CacheResult<()> {
        let policy = self.policy();
        let failure = |_| CacheError::Backend("invalid context".into());
        let context = self.proxies.resolve(req).map_err(failure)?;
        let target = generation::target(req, &policy, context.scheme);
        if target.len() > self.max_key {
            return Err(CacheError::Backend("cache key too large".into()));
        }
        let backend = self.cache.as_ref();
        let result = generation::invalidate(backend, &target, &policy).await;
        if result.is_err() {
            disable(&self.flags);
        }
        result
    }

    fn policy(&self) -> Policy {
        Policy {
            default_ttl: self.default_ttl,
            max_body: self.max_body,
            bypass: Arc::clone(&self.bypass),
            namespace: Arc::clone(&self.namespace),
            proxies: self.proxies.clone(),
            flags: Arc::clone(&self.flags),
            max_key: self.max_key,
            max_headers: self.max_headers,
            max_encoded: self.max_encoded,
            generation_ttl: self.generation_ttl,
            max_new_targets: self.max_new_targets,
        }
    }

    /// Also store responses without `Cache-Control: public`, for `ttl` when
    /// they carry no `max-age`. Meant for a layer on a single route (see the
    /// [type docs](RouteCache)). A `ttl` of zero stores nothing.
    #[must_use]
    pub fn default_ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = Some(ttl);
        self
    }

    /// Bypass the cache for requests carrying `name` (a credential header).
    #[must_use]
    pub fn bypass_header(mut self, name: HeaderName) -> Self {
        Arc::make_mut(&mut self.bypass).push(name);
        self
    }

    /// Store only bodies of at most `bytes` (default
    /// [`DEFAULT_MAX_BODY_BYTES`]).
    #[must_use]
    pub fn max_body_bytes(mut self, bytes: u64) -> Self {
        self.max_body = bytes;
        self
    }
}

/// Per-request view of the layer configuration.
struct Policy {
    default_ttl: Option<Duration>,
    max_body: u64,
    bypass: Arc<Vec<HeaderName>>,
    namespace: Arc<str>,
    proxies: TrustedProxies,
    flags: Arc<AtomicU8>,
    max_key: usize,
    max_headers: usize,
    max_encoded: usize,
    generation_ttl: Duration,
    max_new_targets: u64,
}

impl<C: Cache> Layer<BoxService> for RouteCache<C> {
    type Service = BoxService;

    fn layer(&self, inner: BoxService) -> Self::Service {
        let cache = Arc::clone(&self.cache);
        let policy = Arc::new(self.policy());
        from_fn(move |req: Request, next: Next| {
            let cache = Arc::clone(&cache);
            let policy = Arc::clone(&policy);
            async move { Dispatch { cache, policy }.run(req, next).await }
        })
        .layer(inner)
    }
}

fn is_cacheable_method(method: &Method) -> bool {
    method == Method::GET || method == Method::HEAD
}

fn request_bypasses_cache(headers: &HeaderMap, bypass: &[HeaderName]) -> bool {
    bypass.iter().any(|name| headers.contains_key(name))
        || headers.keys().any(looks_like_credential)
}

/// Header names that commonly carry credentials (`X-Auth-Token`,
/// `X-Session-Id`, `X-Jwt`, `X-Access-Key`, ...). Errs toward bypassing: a
/// false positive only skips the cache, a false negative serves a private
/// response to anyone.
fn looks_like_credential(name: &HeaderName) -> bool {
    const NEEDLES: [&str; 8] = [
        "auth",
        "token",
        "session",
        "jwt",
        "secret",
        "api-key",
        "apikey",
        "access-key",
    ];
    let name = name.as_str();
    NEEDLES.iter().any(|needle| name.contains(needle))
}

/// True when `Vary` names `*` or a header that is not part of the key.
fn varies_outside_key(headers: &HeaderMap) -> bool {
    headers
        .get_all(VARY)
        .iter()
        .flat_map(|value| value.to_str().unwrap_or("*").split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .any(|name| {
            !KEYED_HEADERS
                .iter()
                .any(|keyed| keyed.as_str().eq_ignore_ascii_case(name))
        })
}

/// `Cache-Control` directives as lowercase `(name, value)` pairs.
fn cache_control(headers: &HeaderMap) -> Vec<(String, Option<String>)> {
    headers
        .get_all(CACHE_CONTROL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|text| text.split(','))
        .filter_map(|directive| {
            let (name, value) = match directive.split_once('=') {
                Some((name, value)) => {
                    let value = value.trim().trim_matches('"').to_owned();
                    (name, Some(value))
                }
                None => (directive, None),
            };
            let name = name.trim().trim_matches('"').to_ascii_lowercase();
            (!name.is_empty()).then_some((name, value))
        })
        .collect()
}

/// True when any `Cache-Control` directive is `no-store`, `no-cache` or
/// `private`. This layer never revalidates, so `no-cache` means "do not
/// store" here.
pub(crate) fn cache_control_forbids_store(headers: &HeaderMap) -> bool {
    cache_control(headers)
        .iter()
        .any(|(name, _)| forbids_store(name))
}

fn forbids_store(name: &str) -> bool {
    matches!(name, "no-store" | "no-cache" | "private")
}

/// How long to store `response`, or `None` when it must not be stored.
fn storable_ttl(response: &Response, policy: &Policy) -> Option<Duration> {
    let headers = response.headers();
    let eligible = response.status() == StatusCode::OK
        && !headers.contains_key(SET_COOKIE)
        && !cache_control_forbids_store(headers)
        && !varies_outside_key(headers)
        && !headers.contains_key(header::TRAILER)
        && response
            .body()
            .exact_len()
            .is_some_and(|len| len <= policy.max_body);
    if !eligible {
        return None;
    }
    let directives = cache_control(headers);
    let public = directives.iter().any(|(name, _)| name == "public");
    if !public && policy.default_ttl.is_none() {
        return None;
    }
    let seconds = |wanted: &str| {
        directives
            .iter()
            .find(|(name, _)| name == wanted)
            .and_then(|(_, value)| value.as_deref()?.parse::<u64>().ok())
    };
    let ttl = seconds("s-maxage")
        .or_else(|| seconds("max-age"))
        .map(Duration::from_secs)
        .or(policy.default_ttl)?;
    let remaining = ttl.checked_sub(codec::initial_age(headers)?)?;
    let remaining = remaining.min(policy.generation_ttl);
    (remaining > Duration::ZERO).then_some(remaining)
}

/// Build the cache key from method, scheme, host, path and query, and the
/// keyed headers. Every part is length-prefixed so different requests can
/// never produce the same key.
fn cache_key(req: &Request, scheme: &str) -> String {
    let mut key = String::new();
    push_part(&mut key, req.method().as_str().as_bytes());
    push_part(&mut key, scheme.as_bytes());
    let host = req
        .headers()
        .get(HOST)
        .map(HeaderValue::as_bytes)
        .or_else(|| req.uri().authority().map(|a| a.as_str().as_bytes()))
        .unwrap_or_default();
    push_part(&mut key, &host.to_ascii_lowercase());
    let target = req
        .uri()
        .path_and_query()
        .map_or("/", http::uri::PathAndQuery::as_str);
    push_part(&mut key, target.as_bytes());
    for name in &KEYED_HEADERS {
        let values = req.headers().get_all(name);
        key.push_str(&format!("#{}", values.iter().count()));
        for value in values {
            push_part(&mut key, value.as_bytes());
        }
    }
    key
}

/// Append length-prefixed UTF-8 text or hex-encoded non-text bytes.
fn push_part(key: &mut String, bytes: &[u8]) {
    use std::fmt::Write as _;
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            let _ = write!(key, "s{}:{text}", text.len());
        }
        Err(_) => {
            let _ = write!(key, "x{}:", bytes.len());
            for byte in bytes {
                let _ = write!(key, "{byte:02x}");
            }
        }
    }
}

fn with_x_cache(mut response: Response, value: HeaderValue) -> Response {
    response.headers_mut().insert(X_CACHE, value);
    response
}

/// Rebuild a cached response. A `HEAD` keeps its stored `Content-Length`
/// (the size of the `GET` representation); a `GET` gets its body length.
fn restore(method: &Method, headers: HeaderMap, body: Vec<u8>) -> Response {
    let len = HeaderValue::from(body.len());
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::OK;
    *response.headers_mut() = headers;
    if method != Method::HEAD {
        response.headers_mut().insert(header::CONTENT_LENGTH, len);
    }
    with_x_cache(response, HIT)
}

fn disable(flags: &AtomicU8) {
    flags.fetch_or(DISABLED, Ordering::Release);
    tracing::warn!("cache invalidation failed; cache layer disabled");
}

fn header_bytes(headers: &HeaderMap) -> usize {
    headers.iter().fold(0usize, |size, (name, value)| {
        size.saturating_add(name.as_str().len())
            .saturating_add(value.as_bytes().len())
    })
}

fn unsupported_request(headers: &HeaderMap) -> bool {
    [
        header::RANGE,
        header::IF_RANGE,
        header::IF_MATCH,
        header::IF_NONE_MATCH,
        header::IF_MODIFIED_SINCE,
        header::IF_UNMODIFIED_SINCE,
    ]
    .iter()
    .any(|header| headers.contains_key(header))
}

fn requests_refresh(items: &[Directive], h: &HeaderMap) -> bool {
    let forced = items.iter().any(|(name, _)| needs_refresh(name));
    let pragma = h.get(header::PRAGMA).is_some_and(pragma_no_cache);
    forced || pragma
}

fn needs_refresh(name: &str) -> bool {
    matches!(name, "no-cache" | "max-age" | "min-fresh")
}

fn pragma_no_cache(value: &HeaderValue) -> bool {
    value.as_bytes().eq_ignore_ascii_case(b"no-cache")
}

async fn lookup<C: Cache>(cache: &C, key: &str, policy: &Policy) -> Hit {
    let bytes = cache.get(key).await.ok()??;
    if bytes.len() > policy.max_encoded {
        return None;
    }
    decode(&bytes, policy.max_headers, policy.max_body)
}

/// Where a fill stores: under a known marker, or after admission.
enum Slot {
    Known(String),
    Absent(generation::Started, String),
}

struct Dispatch<C> {
    cache: Arc<C>,
    policy: Arc<Policy>,
}

impl<C: Cache> Dispatch<C> {
    async fn run(self, req: Request, next: Next) -> Response {
        let Self { cache, policy } = self;
        let flags = policy.flags.load(Ordering::Acquire);
        if flags & DISABLED != 0 {
            return with_x_cache(next.run(req).await, MISS);
        }
        let context = match policy.proxies.resolve(&req) {
            Ok(context) => context,
            Err(_) => {
                let error = ApiError::bad_request("Invalid proxy context.");
                return error.into_response();
            }
        };
        let target = generation::target(&req, &policy, context.scheme);
        if target.len() > policy.max_key {
            return with_x_cache(next.run(req).await, MISS);
        }
        if !is_cacheable_method(req.method()) {
            let write = generation::unsafe_method(req.method());
            let invalidate = write && flags & NO_MARKERS == 0;
            let response = next.run(req).await;
            let status = response.status();
            let succeeded = status.is_success() || status.is_redirection();
            if invalidate
                && succeeded
                && generation::invalidate(cache.as_ref(), &target, &policy)
                    .await
                    .is_err()
            {
                // A completed write cannot be undone by a cache failure.
                // Disable this layer rather than reuse stale local data.
                disable(&policy.flags);
            }
            return response;
        }
        let directives = cache_control(req.headers());
        let no_store = directives.iter().any(|(name, _)| name == "no-store");
        let refresh = requests_refresh(&directives, req.headers());
        if no_store
            || unsupported_request(req.headers())
            || request_bypasses_cache(req.headers(), &policy.bypass)
        {
            return with_x_cache(next.run(req).await, MISS);
        }
        if flags & NO_MARKERS != 0 || policy.max_new_targets == 0 {
            // No expiring markers: unsupported backend, zero lifetime or
            // a zero admission budget.
            return with_x_cache(next.run(req).await, MISS);
        }
        let backend = cache.as_ref();
        let known = match generation::peek(backend, &target).await {
            Ok(known) => known,
            Err(_) => return with_x_cache(next.run(req).await, MISS),
        };
        let scheme = context.scheme;
        let (slot, hit) = match known {
            Some(generation) => {
                let key = make_key(&req, &policy, scheme, generation);
                if key.len() > policy.max_key {
                    return with_x_cache(next.run(req).await, MISS);
                }
                let hit = if refresh {
                    None
                } else {
                    lookup(backend, &key, &policy).await
                };
                (Slot::Known(key), hit)
            }
            // No marker: nothing is reachable, and nothing is created
            // until a response is actually stored.
            None => match generation::begin(backend, &policy).await {
                Ok(started) => {
                    let suffix = cache_key(&req, scheme);
                    (Slot::Absent(started, suffix), None)
                }
                Err(_) => return with_x_cache(next.run(req).await, MISS),
            },
        };
        let method = req.method().clone();
        if let Some((headers, body)) = hit {
            return restore(&method, headers, body);
        }
        let response = next.run(req).await;
        let Some(ttl) = storable_ttl(&response, &policy) else {
            return with_x_cache(response, MISS);
        };
        if header_bytes(response.headers()) > policy.max_headers {
            return with_x_cache(response, MISS);
        }
        let key = match slot {
            Slot::Known(key) => Some(key),
            Slot::Absent(started, suffix) => {
                let admitted = generation::admit(backend, &target, &policy, &started);
                match admitted.await {
                    Ok(Some(generation)) => {
                        let key = generation::key_for(&policy, generation, &suffix);
                        (key.len() <= policy.max_key).then_some(key)
                    }
                    _ => None,
                }
            }
        };
        let Some(key) = key else {
            return with_x_cache(response, MISS);
        };
        let (mut parts, body) = response.into_parts();
        let limit = usize::try_from(policy.max_body).unwrap_or(usize::MAX);
        let bytes = match body.into_bytes_limited(limit).await {
            Ok(bytes) => bytes,
            Err(_) => {
                let error = ApiError::internal("Cache body read failed.");
                return error.into_response();
            }
        };
        if let Ok(payload) = encode(&parts.headers, &bytes)
            && payload.len() <= policy.max_encoded
        {
            let _ = cache.set(&key, payload, Some(ttl)).await;
        }
        parts.headers.insert(X_CACHE, MISS);
        Response::from_parts(parts, Body::from(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::try_from(*name).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn cache_control_detects_no_store_no_cache_and_private() {
        assert!(cache_control_forbids_store(&headers(&[(
            "cache-control",
            "no-store"
        )])));
        assert!(cache_control_forbids_store(&headers(&[(
            "cache-control",
            "max-age=0, Private"
        )])));
        assert!(cache_control_forbids_store(&headers(&[(
            "cache-control",
            "public, no-store, max-age=60"
        )])));
        assert!(cache_control_forbids_store(&headers(&[
            ("cache-control", "max-age=60"),
            ("cache-control", "private=\"Set-Cookie\""),
        ])));
        assert!(!cache_control_forbids_store(&headers(&[(
            "cache-control",
            "public, max-age=60"
        )])));
        assert!(cache_control_forbids_store(&headers(&[(
            "cache-control",
            "No-Cache"
        )])));
        assert!(!cache_control_forbids_store(&HeaderMap::new()));
    }

    #[test]
    fn encode_decode_roundtrip_strips_hop_by_hop() {
        let mut map = headers(&[
            ("content-type", "text/plain"),
            ("x-foo", "bar"),
            ("transfer-encoding", "chunked"),
            ("x-cache", "miss"),
        ]);
        map.append("x-foo", HeaderValue::from_static("baz"));
        let encoded = encode(&map, b"hello").unwrap();
        let (decoded, body) = decode(&encoded, 8192, 1024).unwrap();
        assert_eq!(body, b"hello");
        assert_eq!(
            decoded
                .get_all("x-foo")
                .iter()
                .map(|v| v.as_bytes())
                .collect::<Vec<_>>(),
            [b"bar".as_slice(), b"baz".as_slice()]
        );
        assert!(decoded.get("transfer-encoding").is_none());
        assert!(decoded.get("x-cache").is_none());
        assert_eq!(
            decoded.get("content-type").unwrap().as_bytes(),
            b"text/plain"
        );
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode(b"not-json", 8192, 1024).is_none());
        assert!(decode(b"{}", 8192, 1024).is_none());
    }
}
