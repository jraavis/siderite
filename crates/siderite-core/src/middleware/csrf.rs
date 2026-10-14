//! Origin-checking CSRF protection for cookie-authenticated routes.

use super::adapt::{BoxService, Next, from_fn, impl_layer};
use crate::error::ApiError;
use crate::extract::Request;
use crate::response::{IntoResponse, Response};
use http::header::{AUTHORIZATION, COOKIE, ORIGIN, REFERER};
use http::uri::Authority;
use http::{HeaderMap, HeaderName, Method, StatusCode};
use std::sync::Arc;
use thiserror::Error;
use tower::Layer;

const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");
const FORBIDDEN: &str = "Request forbidden.";

/// A configured origin that is not `scheme://host[:port]`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid CSRF origin `{0}`: expected http(s)://host[:port]")]
pub struct CsrfOriginError(String);

/// Rejects cross-site state-changing requests by checking their origin.
///
/// Browsers attach cookies to requests a hostile page triggers, so a route
/// that authenticates with cookies (or HTTP Basic) needs this check. For
/// every method except `GET`, `HEAD`, `OPTIONS` and `TRACE`:
///
/// 1. `Sec-Fetch-Site: cross-site` is rejected, even when `Origin` would
///    match; `Sec-Fetch-Site: same-origin` is accepted.
/// 2. Otherwise `Origin` must equal one of the configured origins. When
///    `Origin` is absent the origin of `Referer` is used instead.
/// 3. `Origin: null`, a missing `Origin` and `Referer`, repeated headers
///    and any other origin are rejected.
///
/// A rejection is one generic `403` problem that does not say which check
/// failed. The allowed origins are configuration only: they are never
/// derived from `Host` or `X-Forwarded-*`, which a client can choose.
///
/// ```
/// use siderite_core::middleware::CsrfOrigin;
///
/// let csrf = CsrfOrigin::new(["https://app.example.com"])?
///     .exempt("/webhooks/");
/// # Ok::<(), siderite_core::middleware::CsrfOriginError>(())
/// ```
#[derive(Debug, Clone)]
pub struct CsrfOrigin {
    origins: Arc<[String]>,
    exempt: Arc<[String]>,
    only_with_cookie: bool,
}

impl CsrfOrigin {
    /// Allow the given exact origins.
    ///
    /// Each origin is `scheme://host[:port]` with an `http` or `https`
    /// scheme and no path, query, fragment or user info. It is lowercased
    /// and a default port (80 for `http`, 443 for `https`) is removed, so
    /// `HTTPS://App.Example.com:443` equals `https://app.example.com`.
    ///
    /// # Errors
    /// [`CsrfOriginError`] for the first origin that does not parse.
    pub fn new<I, S>(origins: I) -> Result<Self, CsrfOriginError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut parsed = Vec::new();
        for raw in origins {
            let raw = raw.as_ref();
            let Some(origin) = parse_origin(raw) else {
                return Err(CsrfOriginError(raw.to_owned()));
            };
            parsed.push(origin);
        }
        Ok(Self {
            origins: parsed.into(),
            exempt: Arc::default(),
            only_with_cookie: false,
        })
    }

    /// Skip the check for paths under `prefix`, for example a webhook.
    ///
    /// The prefix matches whole path segments: `/hooks` covers `/hooks`
    /// and `/hooks/x` but not `/hooksy`. A prefix not starting with `/`
    /// never matches, and paths with `.` or `..` segments are never
    /// exempt. Exempt routes must authenticate callers some other way, for
    /// example by verifying a signature.
    #[must_use]
    pub fn exempt(mut self, prefix: impl Into<String>) -> Self {
        let mut list = self.exempt.to_vec();
        list.push(prefix.into());
        self.exempt = list.into();
        self
    }

    /// Check only requests that carry ambient credentials (default off).
    ///
    /// When on, a request with no `Cookie` header and no
    /// `Authorization: Basic` is not checked. That suits APIs that accept
    /// both bearer tokens and cookies. The trade-off: a request that
    /// arrives without credentials is passed to the handler, so every
    /// route behind this setting must reject anonymous state changes
    /// itself, and any other credential a browser sends by itself would
    /// bypass the check.
    #[must_use]
    pub fn only_with_cookie(mut self) -> Self {
        self.only_with_cookie = true;
        self
    }

    fn is_exempt(&self, path: &str) -> bool {
        if path.split('/').any(|part| part == "." || part == "..") {
            return false;
        }
        self.exempt.iter().any(|prefix| {
            prefix.starts_with('/')
                && path.strip_prefix(prefix.as_str()).is_some_and(|rest| {
                    let boundary = prefix.ends_with('/');
                    rest.is_empty() || boundary || rest.starts_with('/')
                })
        })
    }

    fn permits(&self, req: &Request) -> bool {
        let headers = req.headers();
        if is_safe(req.method()) || self.is_exempt(req.uri().path()) {
            return true;
        }
        if self.only_with_cookie && !has_ambient_credentials(headers) {
            return true;
        }
        match single(headers, &SEC_FETCH_SITE) {
            One::Invalid => return false,
            One::Value(site) if site.eq_ignore_ascii_case("cross-site") => {
                return false;
            }
            One::Value(site) if site.eq_ignore_ascii_case("same-origin") => {
                return true;
            }
            _ => {}
        }
        let claimed = match single(headers, &ORIGIN) {
            One::Invalid => return false,
            One::Value(origin) => parse_origin(origin),
            One::None => match single(headers, &REFERER) {
                One::Value(referer) => referer_origin(referer),
                _ => None,
            },
        };
        claimed.is_some_and(|origin| self.origins.contains(&origin))
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let this = self.clone();
        from_fn(move |req: Request, next: Next| {
            let allowed = this.permits(&req);
            async move {
                if allowed {
                    next.run(req).await
                } else {
                    forbidden()
                }
            }
        })
        .layer(inner)
    }
}

fn forbidden() -> Response {
    ApiError::new(StatusCode::FORBIDDEN, FORBIDDEN).into_response()
}

fn is_safe(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

/// A `Cookie` header, or `Authorization: Basic`, rides along by itself.
fn has_ambient_credentials(headers: &HeaderMap) -> bool {
    if headers.contains_key(COOKIE) {
        return true;
    }
    headers.get_all(AUTHORIZATION).iter().any(|value| {
        value.to_str().ok().is_some_and(|text| {
            let scheme = text.split(' ').next().unwrap_or_default();
            scheme.eq_ignore_ascii_case("basic")
        })
    })
}

enum One<'a> {
    None,
    Value(&'a str),
    Invalid,
}

/// The header's only value; repeated or non-ASCII values are `Invalid`.
fn single<'a>(headers: &'a HeaderMap, name: &HeaderName) -> One<'a> {
    let mut values = headers.get_all(name).iter();
    match (values.next(), values.next()) {
        (None, _) => One::None,
        (Some(value), None) => value.to_str().map_or(One::Invalid, One::Value),
        (Some(_), Some(_)) => One::Invalid,
    }
}

/// Normalize `scheme://host[:port]`; `None` for anything else.
fn parse_origin(raw: &str) -> Option<String> {
    let (scheme, rest) = raw.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', '\\', '%']) {
        return None;
    }
    let authority = rest.parse::<Authority>().ok()?;
    let suffix = rest.strip_prefix(authority.host())?;
    let port = match suffix.strip_prefix(':') {
        Some(digits) if digits.bytes().all(|b| b.is_ascii_digit()) => {
            digits.parse::<u16>().ok().filter(|port| *port != 0)?
        }
        Some(_) => return None,
        None if suffix.is_empty() => default_port,
        None => return None,
    };
    let host = authority.host().to_ascii_lowercase();
    if host.is_empty() {
        None
    } else if port == default_port {
        Some(format!("{scheme}://{host}"))
    } else {
        Some(format!("{scheme}://{host}:{port}"))
    }
}

/// The origin part of a `Referer` URL.
fn referer_origin(referer: &str) -> Option<String> {
    let (scheme, rest) = referer.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    parse_origin(&format!("{scheme}://{}", rest.get(..end)?))
}

impl_layer!(CsrfOrigin);

#[cfg(test)]
#[path = "csrf_tests.rs"]
mod tests;
