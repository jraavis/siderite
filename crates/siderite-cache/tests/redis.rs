//! Live [`RedisCache`] tests.
//!
//! These tests are explicitly ignored in offline runs. Select --ignored
//! with the documented service URL to run them; missing configuration fails.
//! Each test owns a disposable schema/database or a unique Redis namespace.
//! Shared application data and Redis FLUSH commands are not used.
#![cfg(feature = "redis")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::Future;
use std::panic::{AssertUnwindSafe, resume_unwind};
use std::time::Duration;

use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use siderite_backends::redis::RedisStore;
use siderite_cache::{Cache, CacheError, CacheExt, RedisCache};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Widget {
    id: u32,
    name: String,
}

async fn with_cache<F, Fut>(test: F)
where
    F: FnOnce(RedisCache) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(url) = std::env::var("REDIS_URL")
        .ok()
        .filter(|value| value.starts_with("redis"))
    else {
        panic!("live Redis cache tests require REDIS_URL");
    };
    let store = RedisStore::connect(&url).await.unwrap();
    assert_eq!(
        store.database(),
        15,
        "REDIS_URL must select database 15 (selected {})",
        store.database()
    );
    let prefix = format!("siderite-cache-test-{}:", uuid::Uuid::new_v4());
    let cache = RedisCache::new(store.with_prefix(prefix));
    let result = AssertUnwindSafe(test(cache.clone())).catch_unwind().await;
    let cleaned = cache.clear().await;
    if let Err(panic) = result {
        resume_unwind(panic);
    }
    cleaned.unwrap();
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live namespace"]
async fn get_set_delete_clear_and_ttl() {
    with_cache(|cache| async move {
        assert_eq!(cache.get("missing").await.unwrap(), None);
        cache.set("name", b"ada".to_vec(), None).await.unwrap();
        assert_eq!(
            cache.get("name").await.unwrap().as_deref(),
            Some(&b"ada"[..])
        );
        assert!(cache.delete("name").await.unwrap());
        assert!(!cache.delete("name").await.unwrap());

        cache
            .set("ephemeral", b"x".to_vec(), Some(Duration::from_millis(50)))
            .await
            .unwrap();
        assert_eq!(
            cache.get("ephemeral").await.unwrap().as_deref(),
            Some(&b"x"[..])
        );
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(cache.get("ephemeral").await.unwrap(), None);

        cache.set("a", b"1".to_vec(), None).await.unwrap();
        cache.set("b", b"2".to_vec(), None).await.unwrap();
        cache.clear().await.unwrap();
        assert_eq!(cache.get("a").await.unwrap(), None);
        assert_eq!(cache.get("b").await.unwrap(), None);
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live namespace"]
async fn increment_matches_redis_incrby() {
    with_cache(|cache| async move {
        assert_eq!(cache.increment("n", 3).await.unwrap(), 3);
        assert_eq!(cache.increment("n", 2).await.unwrap(), 5);
        assert_eq!(cache.increment("n", -8).await.unwrap(), -3);
        cache.set("s", b"hello".to_vec(), None).await.unwrap();
        assert!(matches!(
            cache.increment("s", 1).await.unwrap_err(),
            CacheError::NotInteger { ref key } if key == "s"
        ));
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live namespace"]
async fn increment_with_expiry_is_atomic_and_expires() {
    with_cache(|cache| async move {
        let ttl = Duration::from_millis(300);
        let mut tasks = Vec::new();
        for _ in 0..32 {
            let cache = cache.clone();
            tasks.push(tokio::spawn(async move {
                cache.increment_with_expiry("c", 1, ttl).await.unwrap()
            }));
        }
        let mut seen = Vec::new();
        for task in tasks {
            seen.push(task.await.unwrap());
        }
        seen.sort_unstable();
        assert_eq!(seen, (1..=32).collect::<Vec<i64>>());
        // The deadline is set once and later increments keep it.
        let long = Duration::from_secs(60);
        tokio::time::sleep(Duration::from_millis(150)).await;
        cache.increment_with_expiry("c", 1, long).await.unwrap();
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(cache.get("c").await.unwrap(), None);
        // A persistent key receives an expiry.
        cache.increment("p", 1).await.unwrap();
        cache.increment_with_expiry("p", 1, ttl).await.unwrap();
        tokio::time::sleep(Duration::from_millis(450)).await;
        assert_eq!(cache.get("p").await.unwrap(), None);
        let zero = cache.increment_with_expiry("z", 1, Duration::ZERO).await;
        assert!(matches!(zero.unwrap_err(), CacheError::InvalidTtl));
        cache.set("s", b"x".to_vec(), None).await.unwrap();
        let bad = cache.increment_with_expiry("s", 1, long).await;
        assert!(matches!(bad.unwrap_err(), CacheError::NotInteger { .. }));
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live namespace"]
async fn json_and_binary_roundtrip() {
    with_cache(|cache| async move {
        let widget = Widget {
            id: 3,
            name: "nes".into(),
        };
        cache.set_json("w", &widget, None).await.unwrap();
        assert_eq!(cache.get_json::<Widget>("w").await.unwrap(), Some(widget));

        let binary: Vec<u8> = (0..=255).collect();
        cache.set("bin", binary.clone(), None).await.unwrap();
        assert_eq!(cache.get("bin").await.unwrap(), Some(binary));
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live namespace"]
async fn clones_share_the_store() {
    with_cache(|cache| async move {
        let clone = cache.clone();
        clone.set("k", b"v".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"v"[..]));
    })
    .await;
}

#[tokio::test]
async fn connect_rejects_a_malformed_url() {
    let err = RedisCache::connect("not a url").await.unwrap_err();
    assert!(matches!(err, CacheError::Backend(_)));
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live namespace"]
async fn empty_prefix_is_replaced_so_clear_is_safe() {
    let Some(url) = std::env::var("REDIS_URL")
        .ok()
        .filter(|value| value.starts_with("redis"))
    else {
        panic!("live Redis cache tests require REDIS_URL");
    };
    let store = RedisStore::connect(&url).await.unwrap();
    assert!(store.prefix().is_empty());
    let cache = RedisCache::new(store);
    assert_eq!(cache.store().prefix(), "siderite-cache:");
}

#[test]
fn redis_cache_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RedisCache>();
}
