//! Live atomic expiring insertion tests for [`RedisCache`].
//!
//! Ignored offline; run with `REDIS_URL` selecting database 15. Each test
//! owns a unique key namespace and never flushes shared data.
#![cfg(feature = "redis")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::Future;
use std::panic::{AssertUnwindSafe, resume_unwind};
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use siderite_backends::redis::{RedisStore, Ttl};
use siderite_cache::{Cache, CacheError, RedisCache};
use tokio::sync::Barrier;

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
    assert_eq!(store.database(), 15, "REDIS_URL must select database 15");
    let prefix = format!("siderite-cache-ttl-{}:", uuid::Uuid::new_v4());
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
async fn creates_with_ttl_then_expires() {
    with_cache(|cache| async move {
        let ttl = Duration::from_secs(2);
        assert!(
            cache
                .set_if_absent_with_ttl("m", vec![0, 255], ttl)
                .await
                .unwrap()
        );
        let Ttl::ExpiresIn(left) = cache.store().ttl("m").await.unwrap() else {
            panic!("key must exist with an expiry");
        };
        assert!(
            !left.is_zero() && left <= ttl,
            "pttl {left:?} not in (0, {ttl:?}]"
        );
        assert_eq!(cache.get("m").await.unwrap(), Some(vec![0, 255]));

        // A loser leaves the winner's value and deadline untouched.
        assert!(
            !cache
                .set_if_absent_with_ttl("m", vec![9], Duration::from_secs(60))
                .await
                .unwrap()
        );
        assert_eq!(cache.get("m").await.unwrap(), Some(vec![0, 255]));
        let Ttl::ExpiresIn(after) = cache.store().ttl("m").await.unwrap() else {
            panic!("key must still expire");
        };
        assert!(after <= left);

        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(cache.get("m").await.unwrap(), None);
        assert!(matches!(
            cache.store().ttl("m").await.unwrap(),
            Ttl::Missing
        ));
        // An expired key counts as absent.
        assert!(
            cache
                .set_if_absent_with_ttl("m", vec![1], ttl)
                .await
                .unwrap()
        );
    })
    .await;
}

#[tokio::test]
#[ignore = "requires explicit REDIS_URL and an isolated live namespace"]
async fn sub_millisecond_ttl_rounds_up_and_invalid_ttls_fail() {
    with_cache(|cache| async move {
        let tiny = Duration::from_nanos(1);
        assert!(
            cache
                .set_if_absent_with_ttl("t", vec![1], tiny)
                .await
                .unwrap()
        );
        match cache.store().ttl("t").await.unwrap() {
            Ttl::ExpiresIn(left) => assert!(
                left <= Duration::from_millis(1),
                "1ns TTL must round up to 1ms, got {left:?}"
            ),
            // The 1ms deadline may already have passed.
            Ttl::Missing => {}
            Ttl::Persistent => panic!("1ns write must carry a TTL"),
        }
        for ttl in [
            Duration::ZERO,
            Duration::MAX,
            Duration::from_millis(u64::MAX),
        ] {
            assert!(matches!(
                cache.set_if_absent_with_ttl("bad", vec![1], ttl).await,
                Err(CacheError::InvalidTtl)
            ));
        }
        assert_eq!(cache.get("bad").await.unwrap(), None);
        // The portable maximum is accepted by Redis and still expires.
        let max = Duration::from_millis(u64::try_from(i64::MAX / 2).unwrap());
        assert!(
            cache
                .set_if_absent_with_ttl("max", vec![1], max)
                .await
                .unwrap()
        );
        assert!(matches!(
            cache.store().ttl("max").await.unwrap(),
            Ttl::ExpiresIn(_)
        ));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit REDIS_URL and an isolated live namespace"]
async fn exactly_one_concurrent_creator_wins() {
    with_cache(|cache| async move {
        const CREATORS: usize = 32;
        for round in 0..5 {
            let key = format!("race-{round}");
            let barrier = Arc::new(Barrier::new(CREATORS));
            let mut tasks = Vec::new();
            for id in 0..CREATORS {
                let (cache, barrier, key) = (cache.clone(), barrier.clone(), key.clone());
                tasks.push(tokio::spawn(async move {
                    barrier.wait().await;
                    let value = vec![u8::try_from(id).unwrap()];
                    let won = cache
                        .set_if_absent_with_ttl(&key, value.clone(), Duration::from_secs(30))
                        .await
                        .unwrap();
                    (won, value)
                }));
            }
            let mut winners = Vec::new();
            for task in tasks {
                let (won, value) = task.await.unwrap();
                if won {
                    winners.push(value);
                }
            }
            assert_eq!(winners.len(), 1, "round {round}: winners {winners:?}");
            assert_eq!(cache.get(&key).await.unwrap(), Some(winners[0].clone()));
            assert!(matches!(
                cache.store().ttl(&key).await.unwrap(),
                Ttl::ExpiresIn(_)
            ));
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit REDIS_URL and an isolated live namespace"]
async fn cancellation_never_leaves_a_non_expiring_key() {
    with_cache(|cache| async move {
        const ATTEMPTS: u64 = 300;
        // `timeout(ZERO)` polls the future once, then drops it: a
        // deterministic cancellation after the command may be queued.
        let mut cancelled = 0;
        for i in 0..ATTEMPTS {
            let key = format!("cancel-{i}");
            let attempt = tokio::time::timeout(
                Duration::ZERO,
                cache.set_if_absent_with_ttl(&key, vec![1], Duration::from_secs(60)),
            )
            .await;
            cancelled += u64::from(attempt.is_err());
        }
        assert!(cancelled > 0, "no attempt was cancelled");
        // Racy budgets cancel at varying points of the exchange.
        for i in 0..ATTEMPTS {
            let key = format!("cancel-race-{i}");
            let budget = Duration::from_micros(i % 120);
            let _ = tokio::time::timeout(
                budget,
                cache.set_if_absent_with_ttl(&key, vec![1], Duration::from_secs(60)),
            )
            .await;
        }
        let mut handles = Vec::new();
        for i in 0..ATTEMPTS {
            let (cache, key) = (cache.clone(), format!("abort-{i}"));
            let handle = tokio::spawn(async move {
                let ttl = Duration::from_secs(60);
                cache.set_if_absent_with_ttl(&key, vec![1], ttl).await
            });
            if i % 2 == 0 {
                tokio::task::yield_now().await;
            }
            handle.abort();
            handles.push(handle);
        }
        for handle in handles {
            let _ = handle.await;
        }
        // Let any command already on the wire land before inspecting.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut present = 0;
        let mut cancelled_present = 0;
        for prefix in ["cancel", "cancel-race", "abort"] {
            for i in 0..ATTEMPTS {
                let key = format!("{prefix}-{i}");
                match cache.store().ttl(&key).await.unwrap() {
                    Ttl::Missing => {}
                    Ttl::ExpiresIn(left) => {
                        present += 1;
                        cancelled_present += u64::from(prefix == "cancel");
                        assert!(left <= Duration::from_secs(60));
                    }
                    Ttl::Persistent => panic!("`{key}` exists without a TTL"),
                }
            }
        }
        // The command still landed after the drop, and with its TTL.
        assert!(
            cancelled_present > 0,
            "no cancelled write landed ({cancelled} cancelled)"
        );
        eprintln!("cancelled={cancelled} keys_present={present}");
    })
    .await;
}
