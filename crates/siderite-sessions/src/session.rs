//! The [`Session`] handle handlers use, and the commit plan it yields.

use crate::config::SessionConfig;
use crate::id::{generate, id_hash};
use crate::store::SessionRecord;
use crate::token;
use http::request::Parts;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use siderite_core::{ApiError, FromRequestParts};
use siderite_openapi::{Operation, SchemaRegistry};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// Name of the documented security scheme (`components.securitySchemes`).
const SCHEME: &str = "sessionCookie";
/// Cookie name the documented scheme describes.
const DOCUMENTED_COOKIE: &str = "__Host-session";

/// A session loaded from the store and ready for the handler.
pub(crate) struct Loaded {
    pub(crate) hash: String,
    pub(crate) record: SessionRecord,
    /// The idle timestamp is due for a write.
    pub(crate) touch: bool,
}

/// What the layer must do after the handler ran.
pub(crate) struct Plan {
    /// Randomness failed: nothing may be stored or issued.
    pub(crate) failed: bool,
    pub(crate) save: Option<Save>,
    /// Hashes to delete once the save succeeded.
    pub(crate) delete: Vec<String>,
    /// Hash of a freshly issued session, to undo if a delete fails.
    pub(crate) fresh: Option<String>,
    /// The user to read the revocation epoch of before saving.
    pub(crate) login: Option<String>,
    pub(crate) cookie: CookieAction,
}

/// One store write.
pub(crate) struct Save {
    pub(crate) hash: String,
    pub(crate) record: SessionRecord,
    pub(crate) ttl: Duration,
    /// A failed write fails the request (otherwise it is only logged).
    pub(crate) required: bool,
    /// The id was loaded from the store, so the write must not recreate
    /// it once another request deleted it.
    pub(crate) existing: bool,
}

/// What to do with the session cookie.
pub(crate) enum CookieAction {
    Keep,
    Set { value: String, max_age: Duration },
    Expire,
}

struct State {
    record: SessionRecord,
    /// Hash of the loaded session still in force.
    existing: Option<String>,
    /// A newly issued id: raw value and hash.
    fresh: Option<(String, String)>,
    /// Hashes of sessions to delete (rotated or logged out).
    stale: Vec<String>,
    dirty: bool,
    touch: bool,
    login: Option<String>,
    failed: bool,
    expire_cookie: bool,
}

impl State {
    fn current_hash(&self) -> Option<&str> {
        match &self.fresh {
            Some((_, hash)) => Some(hash),
            None => self.existing.as_deref(),
        }
    }

    /// Make sure the session has an id, issuing one if it has none.
    fn ensure_id(&mut self) {
        if self.existing.is_none() && self.fresh.is_none() {
            self.issue();
        }
    }

    fn issue(&mut self) {
        match generate() {
            Ok(raw) => {
                let hash = id_hash(&raw);
                self.fresh = Some((raw, hash));
            }
            Err(_) => {
                tracing::error!("session id generation failed");
                self.failed = true;
            }
        }
    }

    /// Replace the id: the old one is deleted when the request ends.
    fn rotate(&mut self) {
        if let Some(old) = self.existing.take() {
            self.stale.push(old);
        }
        self.issue();
        self.dirty = true;
    }
}

/// The request's session, available once a
/// [`SessionLayer`](crate::SessionLayer) is installed.
///
/// Take it as a handler argument. It is cheap to clone and every clone
/// refers to the same session. Changes are written when the handler
/// returns. A request without a session cookie gets no stored session
/// until it inserts data, logs in, rotates or asks for a CSRF token.
///
/// Requests of one session are last-writer-wins: changes made by two
/// concurrent requests are not merged.
#[derive(Clone)]
pub struct Session {
    state: Arc<Mutex<State>>,
    config: Arc<SessionConfig>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never the id or the data.
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl Session {
    pub(crate) fn new(
        config: Arc<SessionConfig>,
        loaded: Option<Loaded>,
        expire_cookie: bool,
    ) -> Self {
        let mut state = State {
            record: SessionRecord::default(),
            existing: None,
            fresh: None,
            stale: Vec::new(),
            dirty: false,
            touch: false,
            login: None,
            failed: false,
            expire_cookie,
        };
        if let Some(loaded) = loaded {
            state.record = loaded.record;
            state.existing = Some(loaded.hash);
            state.touch = loaded.touch;
        }
        Self {
            state: Arc::new(Mutex::new(state)),
            config,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Read the value stored under `key`; `None` when it is absent or is
    /// not a `T`.
    #[must_use]
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let value = self.lock().record.data.get(key)?.clone();
        serde_json::from_value(value).ok()
    }

    /// Store `value` under `key`.
    ///
    /// # Errors
    /// The serialization error when `value` is not representable as JSON.
    pub fn insert<T: Serialize>(
        &self,
        key: impl Into<String>,
        value: T,
    ) -> Result<(), serde_json::Error> {
        let value = serde_json::to_value(value)?;
        let mut state = self.lock();
        state.record.data.insert(key.into(), value);
        state.dirty = true;
        state.ensure_id();
        Ok(())
    }

    /// Remove and return the value under `key`.
    pub fn remove(&self, key: &str) -> Option<Value> {
        let mut state = self.lock();
        let removed = state.record.data.remove(key)?;
        state.dirty = true;
        Some(removed)
    }

    /// The logged-in user, if any.
    #[must_use]
    pub fn user(&self) -> Option<String> {
        self.lock().record.user.clone()
    }

    /// Log `user` in and rotate the session id.
    ///
    /// The old id is deleted when the request ends, with no grace period,
    /// which defeats session fixation. The absolute lifetime restarts. Data
    /// stored before login is kept unless a different user was logged in.
    /// Call it only after the application has authenticated the user.
    pub fn login(&self, user: impl Into<String>) {
        let user = user.into();
        let mut state = self.lock();
        if state.record.user.as_ref().is_some_and(|prev| *prev != user) {
            state.record.data.clear();
        }
        state.record.user = Some(user.clone());
        state.login = Some(user);
        state.rotate();
    }

    /// Delete the session and expire the cookie.
    ///
    /// Anything stored afterwards in the same request starts a new
    /// session.
    pub fn logout(&self) {
        let mut state = self.lock();
        if let Some(old) = state.existing.take() {
            state.stale.push(old);
        }
        state.fresh = None;
        state.record = SessionRecord::default();
        state.dirty = false;
        state.touch = false;
        state.login = None;
        state.expire_cookie = true;
    }

    /// Replace the session id, keeping the data. Use it on any privilege
    /// change and on re-authentication. The old id is deleted when the
    /// request ends.
    pub fn rotate(&self) {
        self.lock().rotate();
    }

    /// The CSRF token of this session: HMAC-SHA256 of the session id hash
    /// under the configured key, base64url. It changes when the id does.
    ///
    /// Returns `None` when no [`csrf_key`](SessionConfig::csrf_key) is
    /// configured. Asking for a token creates the session if the request
    /// had none.
    #[must_use]
    pub fn csrf_token(&self) -> Option<String> {
        let key = self.config.csrf_key.as_deref()?;
        let mut state = self.lock();
        state.ensure_id();
        token::issue(key, state.current_hash()?)
    }

    /// Check a CSRF token from the client against this session, in
    /// constant time. `false` when there is no session or no key.
    #[must_use]
    pub fn verify_csrf(&self, token: &str) -> bool {
        let Some(key) = self.config.csrf_key.as_deref() else {
            return false;
        };
        let state = self.lock();
        state
            .current_hash()
            .is_some_and(|hash| token::verify(key, hash, token))
    }

    /// Whether the request arrived with a live stored session.
    pub(crate) fn is_loaded(&self) -> bool {
        self.lock().existing.is_some()
    }

    /// Decide what to store, delete and set after the handler returned.
    pub(crate) fn plan(&self, now: u64) -> Plan {
        let mut state = self.lock();
        let mut plan = Plan {
            failed: state.failed,
            save: None,
            delete: std::mem::take(&mut state.stale),
            fresh: None,
            login: state.login.take(),
            cookie: CookieAction::Keep,
        };
        if state.failed {
            return plan;
        }
        let mut record = state.record.clone();
        if plan.login.is_some() || record.created_at == 0 {
            record.created_at = now;
        }
        record.last_seen = now;
        let ttl = self.config.ttl(&record, now);
        if let Some((value, hash)) = state.fresh.take() {
            plan.fresh = Some(hash.clone());
            let max_age = self.config.remaining(&record, now);
            plan.cookie = CookieAction::Set { value, max_age };
            plan.save = Some(Save {
                hash,
                record,
                ttl,
                required: true,
                existing: false,
            });
        } else if let Some(hash) = state.existing.clone()
            && (state.dirty || state.touch)
        {
            let required = state.dirty;
            plan.save = Some(Save {
                hash,
                record,
                ttl,
                required,
                existing: true,
            });
        }
        if plan.save.is_none() && state.expire_cookie {
            plan.cookie = CookieAction::Expire;
        }
        plan
    }
}

impl SessionConfig {
    /// Time left of the absolute lifetime, at least one second.
    pub(crate) fn remaining(
        &self,
        record: &SessionRecord,
        now: u64,
    ) -> Duration {
        let age = now.saturating_sub(record.created_at);
        let left = self.absolute.as_secs().saturating_sub(age);
        Duration::from_secs(left.max(1))
    }

    /// How long a store keeps a record written now: the idle timeout plus
    /// one write interval (the stored timestamp can lag that much), but
    /// never past the absolute lifetime.
    fn ttl(&self, record: &SessionRecord, now: u64) -> Duration {
        let idle = self.idle + self.effective_write_interval();
        idle.min(self.remaining(record, now))
    }
}

impl FromRequestParts for Session {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<Session>()
            .cloned()
            .ok_or_else(|| ApiError::internal("SessionLayer is not installed"))
    }

    /// Registers the cookie scheme as an optional requirement: every
    /// request has a session, but only a logged-in one is authenticated.
    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        let definition = json!({
            "type": "apiKey",
            "in": "cookie",
            "name": DOCUMENTED_COOKIE,
        });
        registry.add_security_scheme(SCHEME, definition);
        if op.security.is_empty() {
            op.security.push(BTreeMap::new());
        }
        let requirement = BTreeMap::from([(SCHEME.to_owned(), Vec::new())]);
        if !op.security.contains(&requirement) {
            op.security.push(requirement);
        }
    }
}
