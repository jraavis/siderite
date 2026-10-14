//! [`SessionConfig`]: cookie flags, lifetimes and the CSRF key.

use http::HeaderName;
use siderite_core::SameSite;
use std::fmt;
use std::time::Duration;

const HOST_PREFIX: &str = "__Host-";
const SECURE_PREFIX: &str = "__Secure-";
const MIN_CSRF_KEY: usize = 32;

/// An invalid [`SessionConfig`], reported by
/// [`SessionLayer::new`](crate::SessionLayer::new).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid session configuration: {0}")]
pub struct SessionConfigError(&'static str);

/// How sessions are issued, kept and expired.
///
/// The defaults follow the security notes in `docs/plans/SESSIONS_CSRF.md`:
/// a `__Host-session` cookie that is `HttpOnly`, `Secure`, `Path=/` with
/// no `Domain` and `SameSite=Lax`, a 30 minute idle timeout, a 12 hour
/// absolute lifetime and a 2 second bound on every store call.
#[derive(Clone)]
pub struct SessionConfig {
    pub(crate) name: String,
    pub(crate) secure: bool,
    pub(crate) same_site: SameSite,
    pub(crate) idle: Duration,
    pub(crate) absolute: Duration,
    pub(crate) write_interval: Option<Duration>,
    pub(crate) store_timeout: Duration,
    pub(crate) csrf_key: Option<Vec<u8>>,
    pub(crate) csrf_header: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            name: format!("{HOST_PREFIX}session"),
            secure: true,
            same_site: SameSite::Lax,
            idle: Duration::from_secs(30 * 60),
            absolute: Duration::from_secs(12 * 60 * 60),
            write_interval: None,
            store_timeout: Duration::from_secs(2),
            csrf_key: None,
            csrf_header: "x-csrf-token".to_owned(),
        }
    }
}

impl fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionConfig")
            .field("name", &self.name)
            .field("secure", &self.secure)
            .field("same_site", &self.same_site)
            .field("idle", &self.idle)
            .field("absolute", &self.absolute)
            .field("write_interval", &self.effective_write_interval())
            .field("store_timeout", &self.store_timeout)
            .field("csrf_key", &self.csrf_key.as_ref().map(|_| "[REDACTED]"))
            .field("csrf_header", &self.csrf_header)
            .finish()
    }
}

impl SessionConfig {
    /// The default configuration (see the type docs).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the cookie name.
    ///
    /// A name starting with `__Host-` or `__Secure-` makes browsers insist
    /// on `Secure`, so it cannot be combined with
    /// [`insecure_development`](Self::insecure_development).
    #[must_use]
    pub fn cookie_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set `SameSite`: `Lax` (default), `Strict`, or `None`.
    ///
    /// `None` sends the cookie on every cross-site request, so it is only
    /// accepted together with [`csrf_key`](Self::csrf_key) and `Secure`,
    /// and the layer then requires a valid CSRF token header on every
    /// unsafe request that carries a live session.
    #[must_use]
    pub fn same_site(mut self, same_site: SameSite) -> Self {
        self.same_site = same_site;
        self
    }

    /// Expire a session this long after its last use (default 30 min).
    ///
    /// The last use is recorded at most once per
    /// [`write_interval`](Self::write_interval), so a session can expire up
    /// to one interval early, never late.
    #[must_use]
    pub fn idle_timeout(mut self, idle: Duration) -> Self {
        self.idle = idle;
        self
    }

    /// Expire a session this long after login (default 12 h), however
    /// active it is. Rotation does not extend it; login restarts it.
    #[must_use]
    pub fn absolute_lifetime(mut self, absolute: Duration) -> Self {
        self.absolute = absolute;
        self
    }

    /// Write the idle timestamp at most this often (default a quarter of
    /// the idle timeout), so reads do not become writes.
    #[must_use]
    pub fn write_interval(mut self, interval: Duration) -> Self {
        self.write_interval = Some(interval);
        self
    }

    /// Bound every store call (default 2 s). A call that does not finish
    /// fails like any other store error.
    #[must_use]
    pub fn store_timeout(mut self, timeout: Duration) -> Self {
        self.store_timeout = timeout;
        self
    }

    /// Plain-HTTP development: drop `Secure` and the `__Host-` or
    /// `__Secure-` prefix of the cookie name.
    ///
    /// This is always explicit; it is never inferred from the request.
    /// Never enable it in production.
    #[must_use]
    pub fn insecure_development(mut self) -> Self {
        self.secure = false;
        for prefix in [HOST_PREFIX, SECURE_PREFIX] {
            if let Some(rest) = self.name.strip_prefix(prefix) {
                self.name = rest.to_owned();
                break;
            }
        }
        self
    }

    /// Key for CSRF tokens (at least 32 random bytes, kept secret).
    ///
    /// Without a key [`Session::csrf_token`](crate::Session::csrf_token)
    /// returns `None`.
    #[must_use]
    pub fn csrf_key(mut self, key: impl Into<Vec<u8>>) -> Self {
        self.csrf_key = Some(key.into());
        self
    }

    /// Request header carrying the CSRF token when `SameSite=None`
    /// requires the layer to check it (default `x-csrf-token`).
    #[must_use]
    pub fn csrf_header(mut self, name: impl Into<String>) -> Self {
        self.csrf_header = name.into();
        self
    }

    /// The cookie name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Check the configuration.
    ///
    /// # Errors
    /// [`SessionConfigError`] for an invalid cookie name, a prefixed name
    /// without `Secure`, `SameSite=None` without `Secure` and a CSRF key,
    /// a short key, an invalid header name, or a zero duration.
    pub fn validate(&self) -> Result<(), SessionConfigError> {
        let err = |message| Err(SessionConfigError(message));
        if !is_token(&self.name) {
            return err("cookie name is not a valid token");
        }
        let prefixed = self.name.starts_with(HOST_PREFIX)
            || self.name.starts_with(SECURE_PREFIX);
        if prefixed && !self.secure {
            return err("a prefixed cookie name requires Secure");
        }
        if self.same_site == SameSite::None
            && (!self.secure || self.csrf_key.is_none())
        {
            return err("SameSite=None requires Secure and a csrf_key");
        }
        if self
            .csrf_key
            .as_ref()
            .is_some_and(|k| k.len() < MIN_CSRF_KEY)
        {
            return err("csrf_key must be at least 32 bytes");
        }
        if self.csrf_header.parse::<HeaderName>().is_err() {
            return err("csrf_header is not a valid header name");
        }
        let write = self.effective_write_interval();
        if self.idle.is_zero() || self.absolute.is_zero() {
            return err("session lifetimes must be positive");
        }
        if self.store_timeout.is_zero() || write.is_zero() {
            return err("timeouts and intervals must be positive");
        }
        if write > self.idle {
            return err("write_interval must not exceed idle_timeout");
        }
        Ok(())
    }

    /// The effective idle write interval.
    pub(crate) fn effective_write_interval(&self) -> Duration {
        self.write_interval.unwrap_or(self.idle / 4)
    }
}

/// RFC 6265 cookie names are HTTP tokens.
fn is_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
        })
}
