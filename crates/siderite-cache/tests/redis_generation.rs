//! Explicit live Redis contracts for atomic generations and shared writes.
#![cfg(feature = "redis")]

use futures_util::FutureExt;
use siderite_backends::redis::{RedisStore, Ttl};
use siderite_cache::{Cache, CacheError, RedisCache, RouteCache};
use siderite_core::{App, Cached, get};
use siderite_testkit::TestClient;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn app(cache: RedisCache, value: Arc<AtomicUsize>) -> App {
    let reader = value.clone();
    App::new()
        .route(
            "/item",
            get(move || {
                let reader = reader.clone();
                async move {
                    Cached::public(
                        Duration::from_secs(60),
                        reader.load(Ordering::SeqCst).to_string(),
                    )
                }
            })
            .post(move || {
                let value = value.clone();
                async move {
                    value.fetch_add(1, Ordering::SeqCst);
                    "updated"
                }
            }),
        )
        .layer(RouteCache::new(cache).namespace("shared-contract"))
}

#[tokio::test]
#[ignore = "requires REDIS_URL selecting database 15"]
async fn atomic_insert_and_shared_http_invalidation() -> TestResult {
    let url = std::env::var("REDIS_URL")?;
    let store = RedisStore::connect(&url).await?;
    if store.database() != 15 {
        return Err("select Redis database 15".into());
    }
    let prefix = format!("cache-generation-{}:", uuid::Uuid::new_v4());
    let cache = RedisCache::new(store.with_prefix(prefix));
    let outcome = AssertUnwindSafe(async {
        let mut workers = Vec::new();
        for value in 0..32 {
            let cache = cache.clone();
            workers.push(tokio::spawn(async move {
                cache.set_if_absent("winner", vec![value]).await
            }));
        }
        let mut winners = 0;
        for worker in workers {
            winners += usize::from(worker.await??);
        }
        assert_eq!(winners, 1);
        let state = Arc::new(AtomicUsize::new(0));
        let first = TestClient::try_new(app(cache.clone(), state.clone()))?;
        let second = TestClient::try_new(app(cache.clone(), state))?;
        assert_eq!(first.get("/item").await?.text(), "0");
        let hit = second.get("/item").await?;
        assert_eq!(
            hit.headers
                .get("x-cache")
                .ok_or("missing cache marker")?
                .to_str()?,
            "hit"
        );
        second.post_raw("/item", "text/plain", Vec::new()).await?;
        let fresh = first.get("/item").await?;
        assert_eq!(fresh.text(), "1");
        assert_eq!(
            fresh
                .headers
                .get("x-cache")
                .ok_or("missing refreshed marker")?
                .to_str()?,
            "miss"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .catch_unwind()
    .await;
    cache.clear().await?;
    match outcome {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// Delegates to Redis and remembers every key the layer wrote.
#[derive(Clone)]
struct Spy {
    inner: RedisCache,
    keys: Arc<Mutex<Vec<String>>>,
}

type Bytes = Option<Vec<u8>>;
type Expiry = Option<Duration>;
type Done = Result<(), CacheError>;

#[async_trait::async_trait]
impl Cache for Spy {
    async fn get(&self, key: &str) -> Result<Bytes, CacheError> {
        self.inner.get(key).await
    }

    async fn set(&self, k: &str, v: Vec<u8>, t: Expiry) -> Done {
        self.keys
            .lock()
            .map_err(|e| CacheError::Backend(e.to_string()))?
            .push(k.into());
        self.inner.set(k, v, t).await
    }

    async fn set_if_absent_with_ttl(
        &self,
        k: &str,
        v: Vec<u8>,
        ttl: Duration,
    ) -> Result<bool, CacheError> {
        self.keys
            .lock()
            .map_err(|e| CacheError::Backend(e.to_string()))?
            .push(k.into());
        self.inner.set_if_absent_with_ttl(k, v, ttl).await
    }

    async fn delete(&self, key: &str) -> Result<bool, CacheError> {
        self.inner.delete(key).await
    }

    async fn increment(&self, key: &str, by: i64) -> Result<i64, CacheError> {
        self.inner.increment(key, by).await
    }

    async fn clear(&self) -> Result<(), CacheError> {
        self.inner.clear().await
    }
}

#[tokio::test]
#[ignore = "requires REDIS_URL selecting database 15"]
async fn markers_carry_a_ttl_and_writes_delete() -> TestResult {
    let url = std::env::var("REDIS_URL")?;
    let store = RedisStore::connect(&url).await?;
    if store.database() != 15 {
        return Err("select Redis database 15".into());
    }
    let prefix = format!("cache-generation-ttl-{}:", uuid::Uuid::new_v4());
    let inner = RedisCache::new(store.with_prefix(prefix));
    let spy = Spy {
        inner: inner.clone(),
        keys: Arc::default(),
    };
    let outcome = AssertUnwindSafe(async {
        let state = Arc::new(AtomicUsize::new(0));
        let layer = RouteCache::new(spy.clone())
            .namespace("ttl-contract")
            .generation_ttl(Duration::from_secs(30));
        let reader = state.clone();
        let app = App::new()
            .route(
                "/item",
                get(move || {
                    let reader = reader.clone();
                    async move {
                        Cached::public(
                            Duration::from_secs(3600),
                            reader.load(Ordering::SeqCst).to_string(),
                        )
                    }
                })
                .post(|| async { "ok" }),
            )
            .layer(layer);
        let client = TestClient::try_new(app)?;
        client.get("/item").await?;
        let keys = spy.keys.lock().map_err(|e| e.to_string())?.clone();
        assert_eq!(keys.len(), 2, "one marker and one representation");
        for key in &keys {
            let Ttl::ExpiresIn(left) = inner.store().ttl(key).await? else {
                return Err(format!("{key} has no expiry").into());
            };
            assert!(left <= Duration::from_secs(30), "{key}: {left:?}");
        }
        let marker = &keys[0];
        assert!(inner.get(marker).await?.is_some());
        client.post_raw("/item", "text/plain", Vec::new()).await?;
        assert_eq!(inner.get(marker).await?, None, "write deletes marker");
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .catch_unwind()
    .await;
    inner.clear().await?;
    match outcome {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
