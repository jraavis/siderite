//! Generation lifetime policy: finite markers, clamped fills, fallbacks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use http::{Method, Request};
use siderite_cache::{Cache, CacheError as Error, MemoryCache, RouteCache};
use siderite_core::{App, Body, Cached, State, get};
use siderite_testkit::{TestClient as Client, TestResponse};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type CacheResult<T> = Result<T, Error>;
type Ttl = Option<Duration>;

#[derive(Clone, Default)]
struct Origin {
    value: Arc<AtomicUsize>,
    hits: Arc<AtomicUsize>,
    stall: Arc<AtomicUsize>,
    captured: Arc<Notify>,
    release: Arc<Notify>,
}

async fn read(State(origin): State<Origin>) -> Cached<String> {
    origin.hits.fetch_add(1, Ordering::SeqCst);
    let value = origin.value.load(Ordering::SeqCst);
    if origin.stall.swap(0, Ordering::SeqCst) == 1 {
        origin.captured.notify_one();
        origin.release.notified().await;
    }
    Cached::public(Duration::from_secs(60), value.to_string())
}

async fn write(State(origin): State<Origin>) -> &'static str {
    origin.value.fetch_add(1, Ordering::SeqCst);
    "updated"
}

fn client<C: Cache + Clone>(origin: &Origin, layer: RouteCache<C>) -> Client {
    let app = App::new()
        .route("/item", get(read).post(write))
        .with_state(origin.clone())
        .layer(layer);
    Client::new(app)
}

async fn send(client: &Client, method: Method, refresh: bool) -> TestResponse {
    let mut req = Request::builder().method(method).uri("/item");
    if refresh {
        req = req.header("cache-control", "no-cache");
    }
    let req = req.body(Body::empty()).expect("request");
    client.send(req).await.expect("response")
}

fn marker(response: &TestResponse) -> Option<&str> {
    response.headers.get("x-cache")?.to_str().ok()
}

async fn wait(notify: &Notify) -> TestResult {
    tokio::time::timeout(Duration::from_secs(1), notify.notified()).await?;
    Ok(())
}

#[tokio::test]
async fn marker_expiry_during_a_fill_is_a_safe_miss() -> TestResult {
    let origin = Origin::default();
    let layer = RouteCache::new(MemoryCache::new(64)).generation_ttl(Duration::from_millis(100));
    let client = client(&origin, layer);
    assert_eq!(send(&client, Method::GET, false).await.text(), "0");
    origin.stall.store(1, Ordering::SeqCst);
    let pending = {
        let client = client.clone();
        tokio::spawn(async move { send(&client, Method::GET, true).await })
    };
    wait(&origin.captured).await?;
    // The origin changes and the marker expires while the fill is pending.
    origin.value.store(1, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(250)).await;
    origin.release.notify_one();
    assert_eq!(pending.await?.text(), "0");
    let fresh = send(&client, Method::GET, false).await;
    assert_eq!(fresh.text(), "1", "no stale data after rotation");
    assert_eq!(marker(&fresh), Some("miss"));
    let again = send(&client, Method::GET, false).await;
    assert_eq!((again.text(), marker(&again)), ("1".into(), Some("hit")));
    Ok(())
}

#[tokio::test]
async fn expiry_alone_rotates_and_refills() -> TestResult {
    let origin = Origin::default();
    let layer = RouteCache::new(MemoryCache::new(64)).generation_ttl(Duration::from_millis(80));
    let client = client(&origin, layer);
    send(&client, Method::GET, false).await;
    assert_eq!(
        marker(&send(&client, Method::GET, false).await),
        Some("hit")
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after = send(&client, Method::GET, false).await;
    assert_eq!(marker(&after), Some("miss"));
    assert_eq!(origin.hits.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn concurrent_writes_and_fills_converge() -> TestResult {
    let origin = Origin::default();
    let layer = RouteCache::new(MemoryCache::new(64)).generation_ttl(Duration::from_millis(30));
    let client = client(&origin, layer);
    let mut tasks = Vec::new();
    for round in 0..64 {
        let client = client.clone();
        tasks.push(tokio::spawn(async move {
            if round % 4 == 0 {
                send(&client, Method::POST, false).await;
            } else {
                send(&client, Method::GET, round % 3 == 0).await;
            }
        }));
    }
    for task in tasks {
        task.await?;
    }
    let expected = origin.value.load(Ordering::SeqCst).to_string();
    assert_eq!(send(&client, Method::GET, false).await.text(), expected);
    Ok(())
}

impl Recorder {
    fn new() -> Self {
        Self {
            inner: MemoryCache::new(64),
            markers: Arc::default(),
            fills: Arc::default(),
        }
    }
}

/// Records the TTL of every marker and representation write.
#[derive(Clone)]
struct Recorder {
    inner: MemoryCache,
    markers: Arc<Mutex<Vec<Duration>>>,
    fills: Arc<Mutex<Vec<Ttl>>>,
}

#[async_trait::async_trait]
impl Cache for Recorder {
    async fn get(&self, key: &str) -> CacheResult<Option<Vec<u8>>> {
        self.inner.get(key).await
    }

    async fn set(&self, k: &str, v: Vec<u8>, t: Ttl) -> CacheResult<()> {
        self.fills.lock().expect("lock").push(t);
        self.inner.set(k, v, t).await
    }

    async fn set_if_absent_with_ttl(
        &self,
        key: &str,
        v: Vec<u8>,
        ttl: Duration,
    ) -> CacheResult<bool> {
        self.markers.lock().expect("lock").push(ttl);
        self.inner.set_if_absent_with_ttl(key, v, ttl).await
    }

    async fn delete(&self, key: &str) -> CacheResult<bool> {
        self.inner.delete(key).await
    }

    async fn increment(&self, key: &str, by: i64) -> CacheResult<i64> {
        self.inner.increment(key, by).await
    }

    async fn clear(&self) -> CacheResult<()> {
        self.inner.clear().await
    }
}

#[tokio::test]
async fn representation_ttl_is_clamped_to_the_generation_ttl() -> TestResult {
    let recorder = Recorder::new();
    let cap = Duration::from_secs(5);
    let layer = RouteCache::new(recorder.clone()).generation_ttl(cap);
    let client = client(&Origin::default(), layer);
    send(&client, Method::GET, false).await;
    assert_eq!(*recorder.markers.lock().expect("lock"), [cap]);
    assert_eq!(*recorder.fills.lock().expect("lock"), [Some(cap)]);
    Ok(())
}

#[tokio::test]
async fn default_generation_ttl_is_one_hour() -> TestResult {
    let recorder = Recorder::new();
    let layer = RouteCache::new(recorder.clone());
    let client = client(&Origin::default(), layer);
    send(&client, Method::GET, false).await;
    let hour = Duration::from_secs(3600);
    assert_eq!(*recorder.markers.lock().expect("lock"), [hour]);
    // The 60 s response lifetime is below the cap and is kept.
    let fills = recorder.fills.lock().expect("lock").clone();
    assert_eq!(fills, [Some(Duration::from_secs(60))]);
    Ok(())
}

/// An adapter written before `set_if_absent_with_ttl` existed.
#[derive(Clone)]
struct Legacy {
    inner: MemoryCache,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Cache for Legacy {
    async fn get(&self, key: &str) -> CacheResult<Option<Vec<u8>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.get(key).await
    }

    async fn set(&self, k: &str, v: Vec<u8>, t: Ttl) -> CacheResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.set(k, v, t).await
    }

    async fn set_if_absent(&self, k: &str, v: Vec<u8>) -> CacheResult<bool> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.set_if_absent(k, v).await
    }

    async fn delete(&self, key: &str) -> CacheResult<bool> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.delete(key).await
    }

    async fn increment(&self, key: &str, by: i64) -> CacheResult<i64> {
        self.inner.increment(key, by).await
    }

    async fn clear(&self) -> CacheResult<()> {
        self.inner.clear().await
    }
}

#[tokio::test]
async fn adapters_without_expiring_creation_bypass_without_probing() -> TestResult {
    let origin = Origin::default();
    let legacy = Legacy {
        inner: MemoryCache::new(64),
        calls: Arc::default(),
    };
    let client = client(&origin, RouteCache::new(legacy.clone()));
    for expected in ["0", "0"] {
        let response = send(&client, Method::GET, false).await;
        assert_eq!(response.text(), expected);
        assert_eq!(marker(&response), Some("miss"));
    }
    send(&client, Method::POST, false).await;
    let response = send(&client, Method::GET, false).await;
    assert_eq!(
        (response.text(), marker(&response)),
        ("1".into(), Some("miss"))
    );
    assert_eq!(origin.hits.load(Ordering::SeqCst), 3);
    // Only the first request probed the backend (one get for the marker);
    // nothing non-expiring was ever written and no round trip repeats.
    assert_eq!(legacy.calls.load(Ordering::SeqCst), 1);
    assert_eq!(legacy.inner.get("anything").await?, None);
    Ok(())
}
