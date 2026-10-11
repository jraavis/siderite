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
use siderite_backends::redis::{RedisStore, Ttl};
use siderite_cache::{Admission, AdmitMarker, Cache, CacheError, CacheExt, RedisCache};

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

fn marker_value(admission: &Admission) -> Option<&[u8]> {
    match admission {
        Admission::Marker(token) => Some(token),
        _ => None,
    }
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live namespace"]
async fn admit_marker_checks_epoch_then_creates_once() {
    with_cache(|cache| async move {
        let hour = Duration::from_secs(3600);
        cache.set("epoch", vec![7; 16], Some(hour)).await.unwrap();
        let request = |marker, token: &'static [u8], epoch: &'static [u8]| {
            let base = AdmitMarker::new(marker, token, "epoch", epoch);
            base.budget("budget:", 5, hour)
        };
        let mine: &[u8] = &[1; 16];
        let theirs: &[u8] = &[2; 16];
        let ok: &[u8] = &[7; 16];
        // A different or missing epoch is stale and creates nothing.
        let stale = cache.admit_marker(&request("m", mine, &[8; 16])).await;
        assert_eq!(stale.unwrap(), Admission::Stale);
        let gone = AdmitMarker::new("m", mine, "no-epoch", ok);
        assert_eq!(cache.admit_marker(&gone).await.unwrap(), Admission::Stale);
        assert_eq!(cache.get("m").await.unwrap(), None);
        // The first caller creates the marker; the next gets that value.
        let first = cache.admit_marker(&request("m", mine, ok)).await.unwrap();
        assert_eq!(marker_value(&first), Some(mine));
        let second = cache.admit_marker(&request("m", theirs, ok)).await.unwrap();
        assert_eq!(marker_value(&second), Some(mine));
        // The marker carries the window as its lifetime.
        let Ttl::ExpiresIn(left) = cache.store().ttl("m").await.unwrap() else {
            panic!("marker has no expiry");
        };
        assert!(left <= hour && left > Duration::from_secs(3590));
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live namespace"]
async fn admit_marker_budget_is_exact_under_concurrency_on_the_server_clock() {
    with_cache(|cache| async move {
        let window = Duration::from_secs(600);
        cache.set("epoch", vec![7; 16], Some(window)).await.unwrap();
        let mut tasks = Vec::new();
        for n in 0..64 {
            let cache = cache.clone();
            tasks.push(tokio::spawn(async move {
                let marker = format!("m{n}");
                let token = [u8::try_from(n).unwrap(); 16];
                let base = AdmitMarker::new(&marker, &token, "epoch", &[7; 16]);
                let request = base.budget("budget:", 10, window);
                cache.admit_marker(&request).await.unwrap()
            }));
        }
        let (mut admitted, mut exhausted) = (0, 0);
        for task in tasks {
            match task.await.unwrap() {
                Admission::Marker(_) => admitted += 1,
                Admission::Exhausted { resets_in } => {
                    assert!(resets_in <= window && !resets_in.is_zero());
                    exhausted += 1;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!((admitted, exhausted), (10, 54));
        // Refused callers left no marker behind.
        let mut live = 0;
        for n in 0..64 {
            live += usize::from(cache.get(&format!("m{n}")).await.unwrap().is_some());
        }
        assert_eq!(live, 10);
        // The counter is named after the server window and expires after
        // two windows on the server clock.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let index = now.as_millis() / window.as_millis();
        let mut found = Vec::new();
        for candidate in [index - 1, index, index + 1] {
            let key = format!("budget:{candidate}");
            if let Ttl::ExpiresIn(left) = cache.store().ttl(&key).await.unwrap() {
                found.push(left);
            }
        }
        assert_eq!(found.len(), 1, "one counter for the window: {found:?}");
        let left = found[0];
        assert!(left <= window * 2 && left > window, "{left:?}");
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live namespace"]
async fn invalidate_marker_replaces_the_epoch_and_deletes_the_marker() {
    with_cache(|cache| async move {
        let ttl = Duration::from_secs(30);
        cache.set("marker", vec![1; 16], None).await.unwrap();
        cache
            .invalidate_marker("marker", "epoch", &[9, 0, 255], ttl)
            .await
            .unwrap();
        assert_eq!(cache.get("marker").await.unwrap(), None);
        assert_eq!(cache.get("epoch").await.unwrap(), Some(vec![9, 0, 255]));
        let Ttl::ExpiresIn(left) = cache.store().ttl("epoch").await.unwrap() else {
            panic!("epoch has no expiry");
        };
        assert!(left <= ttl);
        let zero = cache.invalidate_marker("marker", "epoch", &[1], Duration::ZERO);
        assert!(matches!(zero.await.unwrap_err(), CacheError::InvalidTtl));
        // A write is one atomic call: no half-state is observable.
        assert_eq!(cache.get("epoch").await.unwrap(), Some(vec![9, 0, 255]));
    })
    .await;
}

/// `EVALSHA` is used once the script is loaded: no script text is sent.
#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live namespace"]
async fn scripts_run_by_hash() {
    with_cache(|cache| async move {
        let url = std::env::var("REDIS_URL").unwrap();
        let client = redis::Client::open(url).unwrap();
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let hour = Duration::from_secs(3600);
        cache.set("epoch", vec![7; 16], Some(hour)).await.unwrap();
        let run = |n: usize| {
            let cache = cache.clone();
            async move {
                let marker = format!("m{n}");
                let base = AdmitMarker::new(&marker, &[3; 16], "epoch", &[7; 16]);
                let request = base.budget("budget:", 100, hour);
                cache.admit_marker(&request).await.unwrap();
                cache.increment_with_expiry("c", 1, hour).await.unwrap();
            }
        };
        run(0).await;
        let stat = |info: &str, name: &str| -> u64 {
            let line = info.lines().find(|l| l.starts_with(name)).unwrap_or("");
            let calls = line.split("calls=").nth(1).unwrap_or("0");
            calls.split(',').next().unwrap_or("0").parse().unwrap_or(0)
        };
        let before: String = redis::cmd("INFO")
            .arg("commandstats")
            .query_async(&mut conn)
            .await
            .unwrap();
        run(1).await;
        run(2).await;
        let after: String = redis::cmd("INFO")
            .arg("commandstats")
            .query_async(&mut conn)
            .await
            .unwrap();
        let by_hash = stat(&after, "cmdstat_evalsha:") - stat(&before, "cmdstat_evalsha:");
        assert_eq!(by_hash, 4, "two scripts per run, two runs");
        assert_eq!(
            stat(&after, "cmdstat_eval:"),
            stat(&before, "cmdstat_eval:"),
            "no script text is resent"
        );
    })
    .await;
}
