//! Live `CacheStore` tests over Redis.
//!
//! Explicitly ignored in offline runs; select them with `--ignored` and a
//! `REDIS_URL` for database 15. Each test uses a unique key namespace and
//! removes it afterwards; no FLUSH command is sent.
#![cfg(feature = "redis")]
#![allow(clippy::unwrap_used)]

use siderite_backends::redis::RedisStore;
use siderite_cache::{Cache, RedisCache};
use siderite_sessions::{CacheStore, SessionRecord, SessionStore};
use std::time::Duration;

const LONG: Duration = Duration::from_secs(60);

async fn cache() -> RedisCache {
    let Some(url) = std::env::var("REDIS_URL")
        .ok()
        .filter(|value| value.starts_with("redis"))
    else {
        panic!("live Redis session tests require REDIS_URL");
    };
    let store = RedisStore::connect(&url).await.unwrap();
    assert_eq!(store.database(), 15, "REDIS_URL must select database 15");
    let prefix = format!("siderite-session-test-{}:", uuid_like());
    RedisCache::new(store.with_prefix(prefix))
}

fn uuid_like() -> String {
    siderite_sessions::id_hash(&format!("{:?}", std::time::SystemTime::now()))
}

fn record(user: Option<&str>) -> SessionRecord {
    SessionRecord {
        user: user.map(str::to_owned),
        ..SessionRecord::default()
    }
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live namespace"]
async fn redis_sessions_expire_revoke_and_delete() {
    let cache = cache().await;
    let store = CacheStore::new(cache.clone());

    store.save("a", &record(Some("ada")), LONG).await.unwrap();
    store.save("b", &record(Some("bob")), LONG).await.unwrap();
    assert!(store.load("a").await.unwrap().is_some());

    store.revoke_user("ada").await.unwrap();
    assert_eq!(store.auth_epoch("ada").await.unwrap(), 1);
    assert!(store.load("a").await.unwrap().is_none());
    assert!(store.load("b").await.unwrap().is_some());

    store.delete("b").await.unwrap();
    assert!(store.load("b").await.unwrap().is_none());

    let short = Duration::from_millis(100);
    store.save("t", &record(None), short).await.unwrap();
    assert!(store.load("t").await.unwrap().is_some());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(store.load("t").await.unwrap().is_none());

    cache.clear().await.unwrap();
}
