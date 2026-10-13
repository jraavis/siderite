//! Shared harness: a spying store, a test app and cookie helpers.
#![allow(dead_code, clippy::unwrap_used)]

use async_trait::async_trait;
use siderite_core::http::{Method, StatusCode};
use siderite_core::{ApiError, App, Body, FromRequestParts, get, post};
use siderite_sessions::{
    MemoryStore, Session, SessionConfig, SessionLayer, SessionRecord,
    SessionStore, StoreError,
};
use siderite_testkit::{TestClient, TestResponse, http};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A store that records what it is asked and fails or stalls on demand.
#[derive(Clone, Default)]
pub struct Spy {
    pub inner: MemoryStore,
    pub seen: Arc<Mutex<Vec<String>>>,
    pub fail_load: Arc<AtomicBool>,
    pub fail_save: Arc<AtomicBool>,
    pub fail_delete: Arc<AtomicBool>,
    pub delay_ms: Arc<AtomicU64>,
    pub saves: Arc<AtomicUsize>,
    pub loads: Arc<AtomicUsize>,
}

impl Spy {
    pub fn new() -> Self {
        Self {
            inner: MemoryStore::new(1000),
            ..Self::default()
        }
    }

    pub fn flag(flag: &AtomicBool, on: bool) {
        flag.store(on, Ordering::SeqCst);
    }

    pub fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    async fn gate(
        &self,
        id: &str,
        fail: &AtomicBool,
    ) -> Result<(), StoreError> {
        self.seen.lock().unwrap().push(id.to_owned());
        let delay = self.delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        if fail.load(Ordering::SeqCst) {
            return Err(StoreError::new("injected failure"));
        }
        Ok(())
    }
}

#[async_trait]
impl SessionStore for Spy {
    async fn load(
        &self,
        id_hash: &str,
    ) -> Result<Option<SessionRecord>, StoreError> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        self.gate(id_hash, &self.fail_load).await?;
        self.inner.load(id_hash).await
    }

    async fn save(
        &self,
        id_hash: &str,
        record: &SessionRecord,
        ttl: Duration,
    ) -> Result<(), StoreError> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        self.gate(id_hash, &self.fail_save).await?;
        self.inner.save(id_hash, record, ttl).await
    }

    async fn delete(&self, id_hash: &str) -> Result<(), StoreError> {
        self.gate(id_hash, &self.fail_delete).await?;
        self.inner.delete(id_hash).await
    }

    async fn revoke_user(&self, user: &str) -> Result<(), StoreError> {
        self.inner.revoke_user(user).await
    }

    async fn auth_epoch(&self, user: &str) -> Result<u64, StoreError> {
        self.inner.auth_epoch(user).await
    }
}

/// Reads the CSRF token header the way a client would send it.
pub struct Token(pub String);

impl FromRequestParts for Token {
    async fn from_request_parts(
        parts: &mut http::request::Parts,
    ) -> Result<Self, ApiError> {
        let value = parts.headers.get("x-csrf-token");
        let text = value.and_then(|v| v.to_str().ok()).unwrap_or_default();
        Ok(Self(text.to_owned()))
    }
}

/// An application with a small set of session-driven routes.
pub fn app(layer: SessionLayer) -> App {
    App::new()
        .route("/login", post(|s: Session| async move { s.login("ada") }))
        .route(
            "/login-bob",
            post(|s: Session| async move { s.login("bob") }),
        )
        .route("/logout", post(|s: Session| async move { s.logout() }))
        .route("/rotate", post(|s: Session| async move { s.rotate() }))
        .route("/set", post(set))
        .route("/get", get(|s: Session| async move { value(&s) }))
        .route("/me", get(me))
        .route("/token", get(token))
        .route("/verify", post(verify))
        .route("/anon", get(|| async { "no session used" }))
        .layer(layer)
}

fn value(session: &Session) -> String {
    session
        .get::<String>("color")
        .unwrap_or_else(|| "none".into())
}

async fn set(session: Session) -> &'static str {
    session.insert("color", "red").unwrap();
    "set"
}

async fn me(session: Session) -> String {
    session.user().unwrap_or_else(|| "anonymous".into())
}

async fn token(session: Session) -> String {
    session.csrf_token().unwrap_or_else(|| "no-token".into())
}

async fn verify(
    session: Session,
    token: Token,
) -> Result<&'static str, ApiError> {
    if session.verify_csrf(&token.0) {
        Ok("valid")
    } else {
        Err(ApiError::new(StatusCode::FORBIDDEN, "bad token"))
    }
}

pub fn layer(store: impl SessionStore, config: SessionConfig) -> SessionLayer {
    SessionLayer::new(store, config).unwrap()
}

pub fn client(store: impl SessionStore, config: SessionConfig) -> TestClient {
    TestClient::new(app(layer(store, config)))
}

pub async fn send(
    client: &TestClient,
    method: Method,
    path: &str,
    cookie: Option<&str>,
    headers: &[(&str, &str)],
) -> TestResponse {
    let mut req = http::Request::builder().method(method).uri(path);
    if let Some(cookie) = cookie {
        req = req.header("cookie", cookie);
    }
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    client.send(req.body(Body::empty()).unwrap()).await.unwrap()
}

pub async fn get_as(
    client: &TestClient,
    path: &str,
    cookie: Option<&str>,
) -> TestResponse {
    send(client, Method::GET, path, cookie, &[]).await
}

pub async fn post_as(
    client: &TestClient,
    path: &str,
    cookie: Option<&str>,
) -> TestResponse {
    send(client, Method::POST, path, cookie, &[]).await
}

/// All `Set-Cookie` header values.
pub fn set_cookies(res: &TestResponse) -> Vec<String> {
    res.headers
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .collect()
}

/// The single `Set-Cookie` header value.
pub fn set_cookie(res: &TestResponse) -> Option<String> {
    let mut all = set_cookies(res);
    assert!(all.len() <= 1, "more than one Set-Cookie: {all:?}");
    all.pop()
}

/// `name=value` of the cookie a response set.
pub fn pair(res: &TestResponse) -> Option<String> {
    let header = set_cookie(res)?;
    Some(header.split(';').next()?.to_owned())
}

/// The raw session id a response set.
pub fn raw_id(res: &TestResponse) -> Option<String> {
    let pair = pair(res)?;
    pair.split_once('=').map(|(_, id)| id.to_owned())
}

pub fn hash_of(cookie_pair: &str) -> String {
    let (_, id) = cookie_pair.split_once('=').unwrap();
    siderite_sessions::id_hash(id)
}

pub fn problem_body(res: &TestResponse) -> serde_json::Value {
    assert_eq!(res.content_type(), Some("application/problem+json"));
    res.json().unwrap()
}
