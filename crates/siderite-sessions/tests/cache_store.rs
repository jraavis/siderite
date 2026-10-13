//! `CacheStore` over `MemoryCache` and a failing cache.
#![cfg(feature = "cache")]
#![allow(clippy::unwrap_used)]

mod common;

use async_trait::async_trait;
use common::*;
use siderite_cache::{Cache, CacheError, MemoryCache};
use siderite_core::http::StatusCode;
use siderite_sessions::{
    CacheStore, SessionConfig, SessionRecord, SessionStore,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const LONG: Duration = Duration::from_secs(3600);

/// Records every key and can break on demand.
#[derive(Clone)]
struct Probe {
    inner: Arc<MemoryCache>,
    keys: Arc<Mutex<Vec<String>>>,
    broken: Arc<Mutex<bool>>,
}

impl Probe {
    fn new() -> Self {
        Self {
            inner: Arc::new(MemoryCache::new(1000)),
            keys: Arc::default(),
            broken: Arc::default(),
        }
    }

    fn check(&self, key: &str) -> Result<(), CacheError> {
        self.keys.lock().unwrap().push(key.to_owned());
        if *self.broken.lock().unwrap() {
            let secret = "redis://:hunter2@10.0.0.7:6379 refused";
            return Err(CacheError::Backend(secret.into()));
        }
        Ok(())
    }
}

#[async_trait]
impl Cache for Probe {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, CacheError> {
        self.check(key)?;
        self.inner.get(key).await
    }

    async fn set(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<(), CacheError> {
        self.check(key)?;
        self.inner.set(key, value, ttl).await
    }

    async fn delete(&self, key: &str) -> Result<bool, CacheError> {
        self.check(key)?;
        self.inner.delete(key).await
    }

    async fn increment(&self, key: &str, by: i64) -> Result<i64, CacheError> {
        self.check(key)?;
        self.inner.increment(key, by).await
    }

    async fn clear(&self) -> Result<(), CacheError> {
        self.inner.clear().await
    }
}

fn record(user: Option<&str>) -> SessionRecord {
    SessionRecord {
        user: user.map(str::to_owned),
        ..SessionRecord::default()
    }
}

#[tokio::test]
async fn sessions_work_end_to_end_without_raw_ids_in_keys() {
    let probe = Probe::new();
    let client = client(CacheStore::new(probe.clone()), SessionConfig::new());
    let cookie = pair(&post_as(&client, "/login", None).await).unwrap();
    assert_eq!(get_as(&client, "/me", Some(&cookie)).await.text(), "ada");
    let rotated = pair(&post_as(&client, "/rotate", Some(&cookie)).await);
    let rotated = rotated.unwrap();
    assert_eq!(
        get_as(&client, "/me", Some(&cookie)).await.text(),
        "anonymous"
    );
    assert_eq!(get_as(&client, "/me", Some(&rotated)).await.text(), "ada");
    post_as(&client, "/logout", Some(&rotated)).await;
    assert_eq!(
        get_as(&client, "/me", Some(&rotated)).await.text(),
        "anonymous"
    );

    let keys = probe.keys.lock().unwrap().join("\n");
    for cookie in [&cookie, &rotated] {
        let raw = cookie.split_once('=').unwrap().1;
        assert!(!keys.contains(raw), "raw id in a cache key");
        assert!(keys.contains(&hash_of(cookie)));
    }
    assert!(!keys.contains("ada"), "user names are hashed in keys");
    assert!(keys.contains("siderite:session:"));
}

#[tokio::test]
async fn records_expire_with_the_store_ttl() {
    let store = CacheStore::new(MemoryCache::new(10));
    let ttl = Duration::from_millis(40);
    store.save("h", &record(None), ttl).await.unwrap();
    assert!(store.load("h").await.unwrap().is_some());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(store.load("h").await.unwrap().is_none());
}

#[tokio::test]
async fn revocation_is_one_epoch_write_that_rejects_older_records() {
    let probe = Probe::new();
    let store = CacheStore::new(probe.clone());
    assert_eq!(store.auth_epoch("ada").await.unwrap(), 0);
    store.save("a", &record(Some("ada")), LONG).await.unwrap();
    store.save("b", &record(Some("ada")), LONG).await.unwrap();
    store.save("c", &record(Some("bob")), LONG).await.unwrap();
    store.save("d", &record(None), LONG).await.unwrap();
    probe.keys.lock().unwrap().clear();

    store.revoke_user("ada").await.unwrap();
    assert_eq!(probe.keys.lock().unwrap().len(), 1, "a single write");
    assert_eq!(store.auth_epoch("ada").await.unwrap(), 1);
    assert_eq!(store.auth_epoch("bob").await.unwrap(), 0);
    assert!(store.load("a").await.unwrap().is_none());
    assert!(store.load("b").await.unwrap().is_none());
    assert!(store.load("c").await.unwrap().is_some());
    assert!(store.load("d").await.unwrap().is_some());

    // A write that raced the revocation stays invisible.
    store.save("a", &record(Some("ada")), LONG).await.unwrap();
    assert!(store.load("a").await.unwrap().is_none());
    // A login after it carries the new epoch.
    let mut fresh = record(Some("ada"));
    fresh.auth_epoch = store.auth_epoch("ada").await.unwrap();
    store.save("e", &fresh, LONG).await.unwrap();
    assert!(store.load("e").await.unwrap().is_some());
}

#[tokio::test]
async fn revoking_through_the_store_logs_the_user_out_of_the_app() {
    let cache = Arc::new(MemoryCache::new(100));
    let store = CacheStore::new(Arc::clone(&cache));
    let client = client(store.clone(), SessionConfig::new());
    let cookie = pair(&post_as(&client, "/login", None).await).unwrap();
    let bob = pair(&post_as(&client, "/login-bob", None).await).unwrap();
    assert_eq!(get_as(&client, "/me", Some(&cookie)).await.text(), "ada");
    store.revoke_user("ada").await.unwrap();
    let res = get_as(&client, "/me", Some(&cookie)).await;
    assert_eq!(res.text(), "anonymous");
    assert_eq!(get_as(&client, "/me", Some(&bob)).await.text(), "bob");
    let again = pair(&post_as(&client, "/login", None).await).unwrap();
    assert_eq!(get_as(&client, "/me", Some(&again)).await.text(), "ada");
}

#[tokio::test]
async fn cache_errors_fail_closed_without_leaking_their_text() {
    let probe = Probe::new();
    let store = CacheStore::new(probe.clone());
    let client = client(store.clone(), SessionConfig::new());
    let cookie = pair(&post_as(&client, "/login", None).await).unwrap();
    *probe.broken.lock().unwrap() = true;

    let error = store.load("x").await.unwrap_err().to_string();
    for secret in ["hunter2", "10.0.0.7", "redis://"] {
        assert!(!error.contains(secret), "{error}");
    }
    assert!(store.save("x", &record(None), LONG).await.is_err());
    assert!(store.delete("x").await.is_err());
    assert!(store.revoke_user("ada").await.is_err());
    assert!(store.auth_epoch("ada").await.is_err());

    let res = get_as(&client, "/me", Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!res.text().contains("hunter2"));
    let res = post_as(&client, "/login", None).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(set_cookies(&res).is_empty());
}

#[tokio::test]
async fn an_unreadable_record_is_an_error_not_an_anonymous_session() {
    let cache = Arc::new(MemoryCache::new(10));
    let store = CacheStore::new(Arc::clone(&cache));
    cache
        .set("siderite:session:s:h", b"not json".to_vec(), None)
        .await
        .unwrap();
    assert!(store.load("h").await.is_err());
    let user = record(Some("ada"));
    store.save("u", &user, LONG).await.unwrap();
    let epoch_key =
        format!("siderite:session:e:{}", siderite_sessions::id_hash("ada"));
    cache
        .set(&epoch_key, b"garbage".to_vec(), None)
        .await
        .unwrap();
    assert!(store.load("u").await.is_err());
}

#[tokio::test]
async fn prefixes_separate_applications() {
    let cache = Arc::new(MemoryCache::new(10));
    let one = CacheStore::new(Arc::clone(&cache)).with_prefix("one:");
    let two = CacheStore::new(Arc::clone(&cache)).with_prefix("two:");
    one.save("h", &record(None), LONG).await.unwrap();
    assert!(one.load("h").await.unwrap().is_some());
    assert!(two.load("h").await.unwrap().is_none());
}
