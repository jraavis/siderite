//! [`SessionLayer`]: loads, enforces expiry on, and commits sessions.

use crate::config::{SessionConfig, SessionConfigError};
use crate::id::{id_hash, well_formed};
use crate::session::{CookieAction, Loaded, Plan, Session};
use crate::store::{SessionRecord, SessionStore, StoreError};
use http::header::COOKIE;
use http::{HeaderMap, Method, StatusCode};
use siderite_core::middleware::{BoxService, Next, from_fn};
use siderite_core::{
    ApiError, IntoResponse, Request, Response, SameSite, SetCookie,
    WithCookies,
};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tower::Layer;

/// Loads the session named by the request cookie, exposes it to handlers
/// as [`Session`], and writes changes back when the handler returns.
///
/// What the layer guarantees:
///
/// - Idle and absolute expiry are enforced on the server from the stored
///   record; the cookie's own expiry is advisory.
/// - The idle timestamp is written at most once per
///   [`write_interval`](SessionConfig::write_interval), and a request that
///   changed nothing writes nothing else.
/// - Fail closed: when the store cannot be read for a request that carries
///   a session cookie (error or timeout), the request gets a `503`
///   problem and never reaches the handler as an anonymous visitor.
/// - When a write that a request needed (login, rotation, a change) fails,
///   the response becomes `503` and no cookie is set for a session that
///   was not stored.
/// - Every store call is bounded by the
///   [`store_timeout`](SessionConfig::store_timeout).
/// - With `SameSite=None` the layer also requires a valid CSRF token in a
///   header on unsafe requests that carry a live session.
///
/// Register it with `App::layer` after (inside)
/// [`CsrfOrigin`](siderite_core::middleware::CsrfOrigin), so forged
/// requests are refused before any session is loaded.
#[derive(Clone)]
pub struct SessionLayer {
    inner: Arc<Inner>,
}

struct Inner {
    store: Box<dyn SessionStore>,
    config: Arc<SessionConfig>,
}

impl SessionLayer {
    /// Create the layer.
    ///
    /// # Errors
    /// [`SessionConfigError`] when `config` is invalid.
    pub fn new(
        store: impl SessionStore,
        config: SessionConfig,
    ) -> Result<Self, SessionConfigError> {
        config.validate()?;
        Ok(Self {
            inner: Arc::new(Inner {
                store: Box::new(store),
                config: Arc::new(config),
            }),
        })
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let this = Arc::clone(&self.inner);
        from_fn(move |req: Request, next: Next| {
            let this = Arc::clone(&this);
            async move { this.handle(req, next).await }
        })
        .layer(inner)
    }
}

impl Layer<BoxService> for SessionLayer {
    type Service = BoxService;

    fn layer(&self, inner: BoxService) -> BoxService {
        self.wrap(inner)
    }
}

fn unavailable() -> Response {
    ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "Sessions unavailable.")
        .into_response()
}

fn forbidden() -> Response {
    ApiError::new(StatusCode::FORBIDDEN, "Request forbidden.").into_response()
}

fn unix_now() -> u64 {
    // A clock before the epoch makes every session look expired.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(u64::MAX, |elapsed| elapsed.as_secs())
}

fn is_safe(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

/// The first cookie called `name`.
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|line| line.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_owned())
}

impl Inner {
    /// Bound a store call by the configured timeout.
    async fn timed<T>(
        &self,
        call: impl Future<Output = Result<T, StoreError>>,
    ) -> Result<T, StoreError> {
        match tokio::time::timeout(self.config.store_timeout, call).await {
            Ok(result) => result,
            Err(_) => Err(StoreError::new("operation timed out")),
        }
    }

    /// Apply the server-side lifetimes; `Some(touch)` when still alive.
    fn alive(&self, record: &SessionRecord, now: u64) -> Option<bool> {
        let age = now.saturating_sub(record.created_at);
        let idle = now.saturating_sub(record.last_seen);
        let config = &self.config;
        if age >= config.absolute.as_secs() || idle >= config.idle.as_secs() {
            return None;
        }
        Some(idle >= config.effective_write_interval().as_secs())
    }

    /// Resolve the request's cookie. `Err` is the response to send.
    async fn load(
        &self,
        cookie: Option<String>,
        now: u64,
    ) -> Result<(Option<Loaded>, bool), Box<Response>> {
        let Some(raw) = cookie else {
            return Ok((None, false));
        };
        if !well_formed(&raw) {
            return Ok((None, true));
        }
        let hash = id_hash(&raw);
        let record = match self.timed(self.store.load(&hash)).await {
            Ok(Some(record)) => record,
            Ok(None) => return Ok((None, true)),
            Err(error) => {
                tracing::warn!(%error, "session store read failed");
                return Err(Box::new(unavailable()));
            }
        };
        let Some(touch) = self.alive(&record, now) else {
            // Dead by our clock; removing it is housekeeping only.
            let _ = self.timed(self.store.delete(&hash)).await;
            return Ok((None, true));
        };
        Ok((
            Some(Loaded {
                hash,
                record,
                touch,
            }),
            false,
        ))
    }

    /// The token check `SameSite=None` demands of unsafe requests.
    fn token_ok(&self, req: &Request, session: &Session) -> bool {
        let mut values =
            req.headers().get_all(&self.config.csrf_header).iter();
        match (values.next(), values.next()) {
            (Some(value), None) => {
                value.to_str().is_ok_and(|token| session.verify_csrf(token))
            }
            _ => false,
        }
    }

    async fn handle(&self, mut req: Request, next: Next) -> Response {
        let now = unix_now();
        let cookie = cookie_value(req.headers(), &self.config.name);
        let (loaded, expire) = match self.load(cookie, now).await {
            Ok(found) => found,
            Err(response) => return *response,
        };
        let session = Session::new(Arc::clone(&self.config), loaded, expire);
        let enforce = self.config.same_site == SameSite::None
            && !is_safe(req.method())
            && session.is_loaded();
        if enforce && !self.token_ok(&req, &session) {
            return forbidden();
        }
        req.extensions_mut().insert(session.clone());
        let response = next.run(req).await;
        self.commit(session.plan(now), response).await
    }

    /// Store, delete and set the cookie as `plan` says.
    async fn commit(&self, mut plan: Plan, response: Response) -> Response {
        if let Err(error) = self.apply(&mut plan).await {
            tracing::warn!(%error, "session store write failed");
            return unavailable();
        }
        let cookie = match plan.cookie {
            CookieAction::Keep => return response,
            CookieAction::Set { value, max_age } => {
                self.cookie(&value, max_age)
            }
            CookieAction::Expire => self.cookie("", Duration::ZERO),
        };
        WithCookies::new(response).cookie(cookie).into_response()
    }

    async fn apply(&self, plan: &mut Plan) -> Result<(), StoreError> {
        if plan.failed {
            return Err(StoreError::new("session id generation failed"));
        }
        if let Some(save) = plan.save.as_mut() {
            if let Some(user) = &plan.login {
                let epoch = self.timed(self.store.auth_epoch(user)).await?;
                save.record.auth_epoch = epoch;
            }
            let write = self.store.save(&save.hash, &save.record, save.ttl);
            match self.timed(write).await {
                Ok(()) => {}
                Err(error) if save.required => return Err(error),
                Err(error) => {
                    tracing::warn!(%error, "session touch write failed");
                }
            }
        }
        for hash in &plan.delete {
            if let Err(error) = self.timed(self.store.delete(hash)).await {
                // Never leave both the old and the new id live.
                if let Some(fresh) = &plan.fresh {
                    let _ = self.timed(self.store.delete(fresh)).await;
                }
                return Err(error);
            }
        }
        Ok(())
    }

    fn cookie(&self, value: &str, max_age: Duration) -> SetCookie {
        SetCookie::new(self.config.name.clone(), value)
            .http_only(true)
            .secure(self.config.secure)
            .same_site(self.config.same_site)
            .path("/")
            .max_age(max_age)
    }
}
