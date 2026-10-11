//! Refresh, write invalidation, stale-fill and shared-context contracts.

use http::{Method, Request, StatusCode};
use siderite_cache::{Cache, CacheError as Error, MemoryCache, RouteCache};
use siderite_core::{ApiError, App, Body, Cached, State, TrustedProxies, get};
use siderite_testkit::{TestClient as Client, TestResponse};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Ttl = Option<Duration>;
type CacheResult<T> = Result<T, Error>;
type ClientResult = Result<Client, siderite_core::ServerError>;
type Fields<'a> = &'a [(&'a str, &'a str)];
type Peer = axum::extract::ConnectInfo<std::net::SocketAddr>;
type Reply = Result<TestResponse, Box<dyn std::error::Error>>;

#[derive(Clone, Default)]
struct Origin {
    value: Arc<AtomicUsize>,
    hits: Arc<AtomicUsize>,
    slow: Arc<AtomicBool>,
    captured: Arc<Notify>,
    release: Arc<Notify>,
}

async fn read(State(origin): State<Origin>) -> Cached<String> {
    origin.hits.fetch_add(1, Ordering::SeqCst);
    let value = origin.value.load(Ordering::SeqCst);
    if origin.slow.swap(false, Ordering::SeqCst) {
        origin.captured.notify_one();
        origin.release.notified().await;
    }
    Cached::public(Duration::from_secs(60), value.to_string())
}

async fn write(State(origin): State<Origin>) -> &'static str {
    origin.value.fetch_add(1, Ordering::SeqCst);
    "updated"
}

async fn reject() -> Result<&'static str, ApiError> {
    Err(ApiError::bad_request("write rejected"))
}

fn app(origin: Origin, layer: RouteCache<MemoryCache>) -> App {
    App::new()
        .route("/item", get(read).head(read).post(write).put(reject))
        .with_state(origin)
        .layer(layer)
}

fn client_for(origin: Origin) -> Result<Client, siderite_core::ServerError> {
    let layer = RouteCache::new(MemoryCache::new(32));
    Client::try_new(app(origin, layer))
}

async fn call(client: &Client, method: Method, fields: Fields<'_>) -> Reply {
    let mut req = Request::builder().method(method).uri("/item");
    for (name, value) in fields {
        req = req.header(*name, *value);
    }
    Ok(client.send(req.body(Body::empty())?).await?)
}

fn marker(response: &TestResponse) -> Option<&str> {
    response.headers.get("x-cache")?.to_str().ok()
}

async fn wait(notify: &Notify) -> TestResult {
    tokio::time::timeout(Duration::from_secs(1), notify.notified()).await?;
    Ok(())
}

#[tokio::test]
async fn refresh_and_no_store_policy() -> TestResult {
    let origin = Origin::default();
    let client = client_for(origin.clone())?;
    assert_eq!(client.get("/item").await?.text(), "0");
    assert_eq!(marker(&client.get("/item").await?), Some("hit"));
    origin.value.store(1, Ordering::SeqCst);
    let fields = [("cache-control", "no-cache")];
    let fresh = call(&client, Method::GET, &fields).await?;
    assert_eq!(fresh.text(), "1");
    assert_eq!(marker(&fresh), Some("miss"));
    assert_eq!(client.get("/item").await?.text(), "1");
    origin.value.store(2, Ordering::SeqCst);
    let fields = [("cache-control", "no-store")];
    let uncached = call(&client, Method::GET, &fields).await?;
    assert_eq!(uncached.text(), "2");
    assert_eq!(marker(&uncached), Some("miss"));
    assert_eq!(client.get("/item").await?.text(), "1");
    assert_eq!(origin.hits.load(Ordering::SeqCst), 3);
    Ok(())
}

#[tokio::test]
async fn writes_invalidate_all_representations() -> TestResult {
    let origin = Origin::default();
    let client = client_for(origin.clone())?;
    for method in [Method::GET, Method::HEAD] {
        for accept in ["text/plain", "application/json"] {
            let fields = [("accept", accept)];
            call(&client, method.clone(), &fields).await?;
            assert_eq!(
                marker(&call(&client, method.clone(), &fields).await?),
                Some("hit")
            );
        }
    }
    assert_eq!(
        call(&client, Method::PUT, &[]).await?.status,
        StatusCode::BAD_REQUEST
    );
    let fields = [("accept", "text/plain")];
    assert_eq!(
        marker(&call(&client, Method::GET, &fields).await?),
        Some("hit")
    );
    call(&client, Method::POST, &[("authorization", "Bearer test")]).await?;
    for method in [Method::GET, Method::HEAD] {
        for accept in ["text/plain", "application/json"] {
            let fields = [("accept", accept)];
            let reply = call(&client, method.clone(), &fields).await?;
            assert_eq!(marker(&reply), Some("miss"));
            if method == Method::GET {
                assert_eq!(reply.text(), "1");
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn old_inflight_fill_cannot_restore_a_written_target() -> TestResult {
    let origin = Origin::default();
    let client = client_for(origin.clone())?;
    client.get("/item").await?;
    origin.slow.store(true, Ordering::SeqCst);
    let pending_client = client.clone();
    let pending = tokio::spawn(async move {
        let fields = [("cache-control", "no-cache")];
        call(&pending_client, Method::GET, &fields)
            .await
            .map_err(|error| error.to_string())
    });
    wait(&origin.captured).await?;
    call(&client, Method::POST, &[]).await?;
    origin.release.notify_one();
    assert_eq!(pending.await?.map_err(std::io::Error::other)?.text(), "0");
    assert_eq!(client.get("/item").await?.text(), "1");
    assert_eq!(marker(&client.get("/item").await?), Some("hit"));
    Ok(())
}

#[tokio::test]
async fn namespace_isolation_and_shared_writes() -> TestResult {
    let backend = MemoryCache::new(64);
    let a = Origin::default();
    let b = Origin::default();
    b.value.store(99, Ordering::SeqCst);
    let layer = RouteCache::new(backend.clone());
    let first = Client::try_new(app(a.clone(), layer))?;
    let second = Client::try_new(app(b, RouteCache::new(backend.clone())))?;
    assert_eq!(first.get("/item").await?.text(), "0");
    assert_eq!(second.get("/item").await?.text(), "99");
    let layer = RouteCache::new(backend).namespace("shared-v1");
    let first = Client::try_new(app(a.clone(), layer.clone()))?;
    let second = Client::try_new(app(a.clone(), layer))?;
    first.get("/item").await?;
    assert_eq!(marker(&second.get("/item").await?), Some("hit"));
    call(&second, Method::POST, &[]).await?;
    let refreshed = first.get("/item").await?;
    assert_eq!(marker(&refreshed), Some("miss"));
    assert_eq!(refreshed.text(), "1");
    Ok(())
}

#[tokio::test]
async fn explicit_related_target_invalidation_works() -> TestResult {
    let origin = Origin::default();
    let layer = RouteCache::new(MemoryCache::new(16));
    let client = Client::try_new(app(origin.clone(), layer.clone()))?;
    client.get("/item").await?;
    origin.value.store(9, Ordering::SeqCst);
    let target = Request::builder().uri("/item").body(Body::empty())?;
    layer.invalidate_target(&target).await?;
    assert_eq!(client.get("/item").await?.text(), "9");
    Ok(())
}

#[tokio::test]
async fn conditionals_ranges_and_refresh_bypass() -> TestResult {
    let origin = Origin::default();
    let client = client_for(origin.clone())?;
    client.get("/item").await?;
    for field in [
        ("range", "bytes=0-1"),
        ("if-none-match", "\"tag\""),
        ("if-modified-since", "Wed, 21 Oct 2015 07:28:00 GMT"),
        ("cache-control", "max-age=0"),
        ("pragma", "no-cache"),
    ] {
        origin.value.fetch_add(1, Ordering::SeqCst);
        let reply = call(&client, Method::GET, &[field]).await?;
        assert_eq!(marker(&reply), Some("miss"));
    }
    assert_eq!(origin.hits.load(Ordering::SeqCst), 6);
    Ok(())
}

#[tokio::test]
async fn atomic_creation_and_expiry() -> TestResult {
    let cache = Arc::new(MemoryCache::new(8));
    let mut workers = Vec::new();
    for value in 0..32 {
        let cache = cache.clone();
        workers.push(tokio::spawn(async move {
            cache.set_if_absent("one", vec![value]).await
        }));
    }
    let mut winners = 0;
    for worker in workers {
        winners += usize::from(worker.await??);
    }
    assert_eq!(winners, 1);
    cache.set("expired", vec![1], Some(Duration::ZERO)).await?;
    assert!(cache.set_if_absent("expired", vec![2]).await?);
    assert_eq!(cache.get("expired").await?, Some(vec![2]));
    Ok(())
}

#[tokio::test]
async fn cache_scheme_uses_the_same_exact_peer_policy() -> TestResult {
    let origin = Origin::default();
    let peers = TrustedProxies::new([std::net::IpAddr::from([127, 0, 0, 1])]);
    let layer = RouteCache::new(MemoryCache::new(32)).trusted_proxies(peers);
    let client = Client::try_new(app(origin.clone(), layer))?;
    for proto in ["http", "https", "http", "https"] {
        let mut req = Request::builder()
            .uri("/item")
            .header("x-forwarded-proto", proto)
            .body(Body::empty())?;
        req.extensions_mut().insert(axum_peer()?);
        client.send(req).await?;
    }
    assert_eq!(origin.hits.load(Ordering::SeqCst), 2);
    Ok(())
}

fn axum_peer() -> Result<Peer, Box<dyn std::error::Error>> {
    Ok(axum::extract::ConnectInfo("127.0.0.1:4000".parse()?))
}

#[tokio::test]
async fn origin_age_and_hop_fields() -> TestResult {
    use siderite_core::WithHeaders;
    let hits = Arc::new(AtomicUsize::new(0));
    let calls = hits.clone();
    let client = Client::try_new(
        App::new()
            .route(
                "/item",
                get(move || {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        WithHeaders::new("old")
                            .header("cache-control", "public, max-age=60")
                            .header("age", "65")
                    }
                }),
            )
            .layer(RouteCache::new(MemoryCache::new(16))),
    )?;
    client.get("/item").await?;
    assert_eq!(marker(&client.get("/item").await?), Some("miss"));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    let client = Client::try_new(
        App::new()
            .route(
                "/item",
                get(|| async {
                    WithHeaders::new("fresh")
                        .header("cache-control", "public, max-age=60")
                        .header("age", "10")
                        .header("connection", "x-private-hop")
                        .header("x-private-hop", "discard")
                }),
            )
            .layer(RouteCache::new(MemoryCache::new(16))),
    )?;
    client.get("/item").await?;
    let cached = client.get("/item").await?;
    assert_eq!(marker(&cached), Some("hit"));
    assert!(cached.headers.get("x-private-hop").is_none());
    assert!(cached.headers.get("connection").is_none());
    assert!(cached.headers.contains_key(http::header::DATE));
    let age = cached
        .headers
        .get(http::header::AGE)
        .ok_or("missing Age")?
        .to_str()?
        .parse::<u64>()?;
    assert!(age >= 10);
    Ok(())
}

#[tokio::test]
async fn byte_admission_bounds() -> TestResult {
    let encoded = 2_000_000;
    let budgets = [(8, 8192, encoded), (4096, 0, encoded), (4096, 8192, 8)];
    for limits in budgets {
        let origin = Origin::default();
        let layer = RouteCache::new(MemoryCache::new(16));
        let layer = layer.byte_limits(limits.0, limits.1, limits.2);
        let client = Client::try_new(app(origin.clone(), layer))?;
        client.get("/item").await?;
        assert_eq!(marker(&client.get("/item").await?), Some("miss"));
        assert_eq!(origin.hits.load(Ordering::SeqCst), 2);
    }
    Ok(())
}

#[derive(Clone)]
struct FaultCache {
    inner: MemoryCache,
    fail_writes: Arc<AtomicBool>,
    evict_marker: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Cache for FaultCache {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        let marker = key.starts_with("route-v2-target:");
        if marker && self.evict_marker.swap(false, Ordering::SeqCst) {
            self.inner.delete(key).await?;
        }
        self.inner.get(key).await
    }

    async fn set(&self, k: &str, v: Vec<u8>, t: Ttl) -> CacheResult<()> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(Error::Backend("injected".into()));
        }
        self.inner.set(k, v, t).await
    }

    async fn set_if_absent(&self, key: &str, v: Vec<u8>) -> CacheResult<bool> {
        self.inner.set_if_absent(key, v).await
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
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(Error::Backend("injected".into()));
        }
        self.inner.delete(key).await
    }

    async fn increment(&self, key: &str, by: i64) -> Result<i64, Error> {
        self.inner.increment(key, by).await
    }

    async fn increment_with_expiry(&self, key: &str, by: i64, ttl: Duration) -> Result<i64, Error> {
        self.inner.increment_with_expiry(key, by, ttl).await
    }

    async fn clear(&self) -> CacheResult<()> {
        self.inner.clear().await
    }
}

fn fault_client(origin: Origin, backend: FaultCache) -> ClientResult {
    Client::try_new(
        App::new()
            .route("/item", get(read).post(write))
            .with_state(origin)
            .layer(RouteCache::new(backend)),
    )
}

fn fault_cache() -> FaultCache {
    FaultCache {
        inner: MemoryCache::new(32),
        fail_writes: Arc::new(AtomicBool::new(false)),
        evict_marker: Arc::new(AtomicBool::new(false)),
    }
}

#[tokio::test]
async fn generation_eviction_never_revives_entries() -> TestResult {
    let origin = Origin::default();
    let backend = fault_cache();
    let client = fault_client(origin.clone(), backend.clone())?;
    client.get("/item").await?;
    assert_eq!(marker(&client.get("/item").await?), Some("hit"));
    origin.value.store(8, Ordering::SeqCst);
    backend.evict_marker.store(true, Ordering::SeqCst);
    let refreshed = client.get("/item").await?;
    assert_eq!(refreshed.text(), "8");
    assert_eq!(marker(&refreshed), Some("miss"));
    assert_eq!(client.get("/item").await?.text(), "8");
    Ok(())
}

#[tokio::test]
async fn invalidation_fault_disables_stale_reuse() -> TestResult {
    let origin = Origin::default();
    let backend = fault_cache();
    let client = fault_client(origin, backend.clone())?;
    client.get("/item").await?;
    backend.fail_writes.store(true, Ordering::SeqCst);
    assert_eq!(
        call(&client, Method::POST, &[]).await?.status,
        StatusCode::OK
    );
    backend.fail_writes.store(false, Ordering::SeqCst);
    let refreshed = client.get("/item").await?;
    assert_eq!(refreshed.text(), "1");
    assert_eq!(marker(&refreshed), Some("miss"));
    assert_eq!(marker(&client.get("/item").await?), Some("miss"));
    Ok(())
}
