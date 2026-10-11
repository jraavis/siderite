//! Admission and invalidation edge cases of the route cache: epoch
//! eviction, writes racing an admission, epoch stripes, partial adapters,
//! backend failures and the local spent-budget note.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use http::{Method, Request};
use siderite_cache::{Admission, AdmitMarker, Cache, CacheError, MemoryCache, RouteCache};
use siderite_core::{App, Body, Cached, State, get};
use siderite_testkit::{TestClient as Client, TestResponse};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::Notify;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type CacheResult<T> = Result<T, CacheError>;
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

fn app<C: Cache + Clone>(origin: &Origin, layer: RouteCache<C>) -> App {
    App::new()
        .route("/item", get(read).post(write))
        .with_state(origin.clone())
        .layer(layer)
}

fn client<C: Cache + Clone>(origin: &Origin, layer: RouteCache<C>) -> Client {
    Client::new(app(origin, layer))
}

async fn call(client: &Client, method: Method, target: &str) -> TestResponse {
    let req = Request::builder().method(method).uri(target);
    client.send(req.body(Body::empty()).unwrap()).await.unwrap()
}

async fn fetch(client: &Client, target: &str) -> (String, String) {
    let response = call(client, Method::GET, target).await;
    let cache = response.headers.get("x-cache").unwrap();
    (response.text(), cache.to_str().unwrap().to_owned())
}

async fn wait(notify: &Notify) -> TestResult {
    tokio::time::timeout(Duration::from_secs(1), notify.notified()).await?;
    Ok(())
}

fn text(value: &str, cache: &str) -> (String, String) {
    (value.to_owned(), cache.to_owned())
}

/// A GET held in the origin, a write, enough unrelated traffic to evict the
/// write's epoch from a tiny cache, then the release: the stale fill must
/// not become reachable.
#[tokio::test]
async fn evicted_epoch_cannot_revive_a_stale_fill() -> TestResult {
    let origin = Origin::default();
    let cache = MemoryCache::new(8);
    let client = client(&origin, RouteCache::new(cache.clone()));
    origin.stall.store(1, Ordering::SeqCst);
    let pending = {
        let client = client.clone();
        tokio::spawn(async move { fetch(&client, "/item").await })
    };
    wait(&origin.captured).await?;
    call(&client, Method::POST, "/item").await;
    for n in 0..16 {
        cache.set(&format!("unrelated-{n}"), vec![1], None).await?;
    }
    origin.release.notify_one();
    assert_eq!(pending.await?, text("0", "miss"));
    assert_eq!(fetch(&client, "/item").await, text("1", "miss"));
    assert_eq!(fetch(&client, "/item").await, text("1", "hit"));
    Ok(())
}

/// Generic admission (an adapter with only the primitives) runs a write
/// between creating the marker and comparing the epoch.
#[derive(Clone)]
struct Raced {
    inner: MemoryCache,
    client: Arc<OnceLock<Client>>,
    armed: Arc<AtomicBool>,
}

#[async_trait]
impl Cache for Raced {
    async fn get(&self, key: &str) -> CacheResult<Option<Vec<u8>>> {
        self.inner.get(key).await
    }

    async fn set(&self, k: &str, v: Vec<u8>, t: Ttl) -> CacheResult<()> {
        self.inner.set(k, v, t).await
    }

    async fn set_if_absent_with_ttl(
        &self,
        key: &str,
        v: Vec<u8>,
        ttl: Duration,
    ) -> CacheResult<bool> {
        self.inner.set_if_absent_with_ttl(key, v, ttl).await
    }

    async fn delete(&self, key: &str) -> CacheResult<bool> {
        self.inner.delete(key).await
    }

    async fn increment(&self, key: &str, by: i64) -> CacheResult<i64> {
        self.inner.increment(key, by).await
    }

    async fn increment_with_expiry(&self, key: &str, by: i64, ttl: Duration) -> CacheResult<i64> {
        // Runs right after the marker is created, before the epoch check.
        if self.armed.swap(false, Ordering::SeqCst) {
            let client = self.client.get().unwrap();
            call(client, Method::POST, "/item").await;
        }
        self.inner.increment_with_expiry(key, by, ttl).await
    }

    async fn clear(&self) -> CacheResult<()> {
        self.inner.clear().await
    }
}

#[tokio::test]
async fn write_between_marker_creation_and_epoch_check_refuses_the_fill() -> TestResult {
    let origin = Origin::default();
    let raced = Raced {
        inner: MemoryCache::new(64),
        client: Arc::default(),
        armed: Arc::default(),
    };
    let client = client(&origin, RouteCache::new(raced.clone()));
    raced.client.set(client.clone()).ok();
    raced.armed.store(true, Ordering::SeqCst);
    // The origin read "0", then the write lands before the epoch check.
    assert_eq!(fetch(&client, "/item").await, text("0", "miss"));
    assert_eq!(fetch(&client, "/item").await, text("1", "miss"));
    assert_eq!(fetch(&client, "/item").await, text("1", "hit"));
    Ok(())
}

/// Records the epoch key each write touches, so tests can choose targets
/// that share a stripe and targets that do not.
#[derive(Clone)]
struct Stripes {
    inner: MemoryCache,
    last: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl Cache for Stripes {
    async fn get(&self, key: &str) -> CacheResult<Option<Vec<u8>>> {
        self.inner.get(key).await
    }

    async fn set(&self, k: &str, v: Vec<u8>, t: Ttl) -> CacheResult<()> {
        if k.starts_with("route-v2-epoch:") {
            *self.last.lock().unwrap() = Some(k.to_owned());
        }
        self.inner.set(k, v, t).await
    }

    async fn set_if_absent_with_ttl(
        &self,
        key: &str,
        v: Vec<u8>,
        ttl: Duration,
    ) -> CacheResult<bool> {
        self.inner.set_if_absent_with_ttl(key, v, ttl).await
    }

    async fn delete(&self, key: &str) -> CacheResult<bool> {
        self.inner.delete(key).await
    }

    async fn increment(&self, key: &str, by: i64) -> CacheResult<i64> {
        self.inner.increment(key, by).await
    }

    async fn increment_with_expiry(&self, key: &str, by: i64, ttl: Duration) -> CacheResult<i64> {
        self.inner.increment_with_expiry(key, by, ttl).await
    }

    async fn clear(&self) -> CacheResult<()> {
        self.inner.clear().await
    }
}

/// Returns a target, one that shares its stripe, and one that does not.
async fn stripe_triple(client: &Client, spy: &Stripes) -> (String, String, String) {
    let mut by_stripe: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for n in 0..200 {
        let target = format!("/item?n={n}");
        call(client, Method::POST, &target).await;
        let key = spy.last.lock().unwrap().take().unwrap();
        by_stripe.entry(key).or_default().push(target);
    }
    assert!(by_stripe.len() > 1, "targets spread over stripes");
    let shared = by_stripe.values().find(|group| group.len() > 1).unwrap();
    let other = by_stripe
        .values()
        .find(|group| group[0] != shared[0])
        .unwrap();
    (shared[0].clone(), shared[1].clone(), other[0].clone())
}

/// A write refuses first fills of targets in its stripe and no others.
#[tokio::test]
async fn writes_refuse_first_fills_only_in_their_stripe() -> TestResult {
    let origin = Origin::default();
    let spy = Stripes {
        inner: MemoryCache::new(4096),
        last: Arc::default(),
    };
    let client = client(&origin, RouteCache::new(spy.clone()));
    let (target, same, other) = stripe_triple(&client, &spy).await;
    for (written, stored) in [(&other, "hit"), (&same, "miss")] {
        spy.inner.clear().await?;
        origin.stall.store(1, Ordering::SeqCst);
        let pending = {
            let (client, target) = (client.clone(), target.clone());
            tokio::spawn(async move { fetch(&client, &target).await })
        };
        wait(&origin.captured).await?;
        call(&client, Method::POST, written).await;
        origin.release.notify_one();
        assert_eq!(pending.await?.1, "miss");
        let after = fetch(&client, &target).await.1;
        assert_eq!(after, stored, "write to {written}");
    }
    Ok(())
}

/// Counts and filters calls; each flag withholds one capability.
#[derive(Clone, Default)]
struct Partial {
    inner: Option<MemoryCache>,
    calls: Arc<Mutex<Vec<String>>>,
    no_increment: bool,
    no_insert: bool,
}

impl Partial {
    fn new(no_increment: bool, no_insert: bool) -> Self {
        Self {
            inner: Some(MemoryCache::new(256)),
            no_increment,
            no_insert,
            ..Self::default()
        }
    }

    fn mem(&self) -> &MemoryCache {
        self.inner.as_ref().unwrap()
    }

    fn note(&self, what: &str) {
        self.calls.lock().unwrap().push(what.to_owned());
    }

    fn count(&self, what: &str) -> usize {
        let calls = self.calls.lock().unwrap();
        calls.iter().filter(|call| call.starts_with(what)).count()
    }
}

#[async_trait]
impl Cache for Partial {
    async fn get(&self, key: &str) -> CacheResult<Option<Vec<u8>>> {
        self.note(&format!("get {key}"));
        self.mem().get(key).await
    }

    async fn set(&self, k: &str, v: Vec<u8>, t: Ttl) -> CacheResult<()> {
        self.note(&format!("set {k}"));
        self.mem().set(k, v, t).await
    }

    async fn set_if_absent_with_ttl(
        &self,
        key: &str,
        v: Vec<u8>,
        ttl: Duration,
    ) -> CacheResult<bool> {
        if self.no_insert {
            return Err(CacheError::Unsupported {
                operation: "set_if_absent_with_ttl",
            });
        }
        self.note(&format!("insert {key}"));
        self.mem().set_if_absent_with_ttl(key, v, ttl).await
    }

    async fn delete(&self, key: &str) -> CacheResult<bool> {
        self.note(&format!("delete {key}"));
        self.mem().delete(key).await
    }

    async fn increment(&self, key: &str, by: i64) -> CacheResult<i64> {
        self.mem().increment(key, by).await
    }

    async fn increment_with_expiry(&self, key: &str, by: i64, ttl: Duration) -> CacheResult<i64> {
        if self.no_increment {
            return Err(CacheError::Unsupported {
                operation: "increment_with_expiry",
            });
        }
        self.note(&format!("increment {key}"));
        self.mem().increment_with_expiry(key, by, ttl).await
    }

    async fn clear(&self) -> CacheResult<()> {
        self.mem().clear().await
    }
}

/// An adapter with the expiring insert but no increment-with-expiry: the
/// first store attempt fails safely, leaves no marker, and later requests
/// skip the backend.
#[tokio::test]
async fn insert_without_increment_bypasses_and_leaves_no_marker() -> TestResult {
    let origin = Origin::default();
    let partial = Partial::new(true, false);
    let client = client(&origin, RouteCache::new(partial.clone()));
    assert_eq!(fetch(&client, "/item").await, text("0", "miss"));
    // The marker the failed admission created was removed again.
    assert_eq!(partial.count("insert route-v2-target:"), 1);
    assert_eq!(partial.count("delete route-v2-target:"), 1);
    assert_eq!(partial.count("increment"), 0);
    let marker = partial.calls.lock().unwrap().iter().find_map(|call| {
        call.strip_prefix("insert route-v2-target:")
            .map(str::to_owned)
    });
    let key = format!("route-v2-target:{}", marker.unwrap());
    assert_eq!(partial.mem().get(&key).await?, None);
    // After the first Unsupported the layer stops probing the backend.
    let before = partial.calls.lock().unwrap().len();
    assert_eq!(fetch(&client, "/item").await, text("0", "miss"));
    call(&client, Method::POST, "/item").await;
    assert_eq!(partial.calls.lock().unwrap().len(), before);
    Ok(())
}

/// The opposite adapter fails before any budget is spent.
#[tokio::test]
async fn increment_without_insert_spends_no_budget() -> TestResult {
    let origin = Origin::default();
    let partial = Partial::new(false, true);
    let client = client(&origin, RouteCache::new(partial.clone()));
    assert_eq!(fetch(&client, "/item").await, text("0", "miss"));
    assert_eq!(partial.count("increment"), 0);
    assert_eq!(partial.count("insert"), 0);
    assert_eq!(fetch(&client, "/item").await, text("0", "miss"));
    assert_eq!(origin.hits.load(Ordering::SeqCst), 2);
    Ok(())
}

/// Fails reads of the epoch or the admission on demand.
#[derive(Clone)]
struct Flaky {
    inner: MemoryCache,
    fail_epoch: Arc<AtomicBool>,
    fail_admit: Arc<AtomicBool>,
}

impl Flaky {
    fn error() -> CacheError {
        CacheError::Backend("injected failure".into())
    }
}

#[async_trait]
impl Cache for Flaky {
    async fn get(&self, key: &str) -> CacheResult<Option<Vec<u8>>> {
        let epoch = key.starts_with("route-v2-epoch:");
        if epoch && self.fail_epoch.load(Ordering::SeqCst) {
            return Err(Self::error());
        }
        self.inner.get(key).await
    }

    async fn set(&self, k: &str, v: Vec<u8>, t: Ttl) -> CacheResult<()> {
        self.inner.set(k, v, t).await
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

    async fn admit_marker(&self, req: &AdmitMarker<'_>) -> CacheResult<Admission> {
        if self.fail_admit.load(Ordering::SeqCst) {
            return Err(Self::error());
        }
        self.inner.admit_marker(req).await
    }

    async fn set_if_absent_with_ttl(
        &self,
        key: &str,
        v: Vec<u8>,
        ttl: Duration,
    ) -> CacheResult<bool> {
        self.inner.set_if_absent_with_ttl(key, v, ttl).await
    }
}

/// Backend errors while reading the epoch or admitting are safe misses:
/// the origin is served, nothing is stored, and the layer recovers.
#[tokio::test]
async fn backend_errors_during_admission_are_safe_misses() -> TestResult {
    let origin = Origin::default();
    let flaky = Flaky {
        inner: MemoryCache::new(64),
        fail_epoch: Arc::default(),
        fail_admit: Arc::default(),
    };
    let client = client(&origin, RouteCache::new(flaky.clone()));
    flaky.fail_admit.store(true, Ordering::SeqCst);
    for _ in 0..2 {
        assert_eq!(fetch(&client, "/item").await, text("0", "miss"));
    }
    flaky.fail_admit.store(false, Ordering::SeqCst);
    flaky.fail_epoch.store(true, Ordering::SeqCst);
    assert_eq!(fetch(&client, "/item").await, text("0", "miss"));
    assert_eq!(origin.hits.load(Ordering::SeqCst), 3);
    // Healthy again: the layer was not disabled and nothing stale exists.
    flaky.fail_epoch.store(false, Ordering::SeqCst);
    assert_eq!(fetch(&client, "/item").await, text("0", "miss"));
    assert_eq!(fetch(&client, "/item").await, text("0", "hit"));
    Ok(())
}

/// Forwards to `MemoryCache` and counts every call.
#[derive(Clone)]
struct Counting {
    inner: MemoryCache,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Cache for Counting {
    async fn get(&self, key: &str) -> CacheResult<Option<Vec<u8>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.get(key).await
    }

    async fn set(&self, k: &str, v: Vec<u8>, t: Ttl) -> CacheResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.set(k, v, t).await
    }

    async fn set_if_absent_with_ttl(
        &self,
        key: &str,
        v: Vec<u8>,
        ttl: Duration,
    ) -> CacheResult<bool> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.set_if_absent_with_ttl(key, v, ttl).await
    }

    async fn delete(&self, key: &str) -> CacheResult<bool> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.delete(key).await
    }

    async fn increment(&self, key: &str, by: i64) -> CacheResult<i64> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.increment(key, by).await
    }

    async fn clear(&self) -> CacheResult<()> {
        self.inner.clear().await
    }

    async fn admit_marker(&self, req: &AdmitMarker<'_>) -> CacheResult<Admission> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.admit_marker(req).await
    }

    async fn invalidate_marker(
        &self,
        marker: &str,
        epoch_key: &str,
        epoch: &[u8],
        ttl: Duration,
    ) -> CacheResult<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner
            .invalidate_marker(marker, epoch_key, epoch, ttl)
            .await
    }
}

#[tokio::test]
async fn call_counts_per_path_are_minimal() -> TestResult {
    let origin = Origin::default();
    let counting = Counting {
        inner: MemoryCache::new(256),
        calls: Arc::default(),
    };
    let layer = RouteCache::new(counting.clone()).max_new_targets(1);
    let client = client(&origin, layer);
    let calls = || counting.calls.swap(0, Ordering::SeqCst);
    // First request: marker read, epoch read, epoch create, admit, store.
    fetch(&client, "/item?n=0").await;
    assert_eq!(calls(), 5);
    // Hit: one read of the marker, one of the representation.
    assert_eq!(fetch(&client, "/item?n=0").await.1, "hit");
    assert_eq!(calls(), 2);
    // Budget spent: the first refusal costs the marker read, the epoch
    // read (plus its creation in a stripe not yet used) and the admission.
    assert_eq!(fetch(&client, "/item?n=1").await.1, "miss");
    assert!((3..=4).contains(&calls()));
    // ... then the layer remembers and makes only the marker read.
    for n in 2..6 {
        assert_eq!(fetch(&client, &format!("/item?n={n}")).await.1, "miss");
        assert_eq!(calls(), 1, "over-budget request {n}");
    }
    // A write is a single backend call.
    call(&client, Method::POST, "/item?n=0").await;
    assert_eq!(calls(), 1);
    Ok(())
}
