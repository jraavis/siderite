//! Load regression for the metadata bound (admission budget, R05).
//!
//! Sustained distinct targets, uncacheable responses and namespace churn
//! must keep marker and counter keys within the documented bound, on
//! `MemoryCache` and (ignored, `REDIS_URL`, database 15) live Redis.
//! `cargo test -p siderite-cache --test metadata_bound -- --nocapture`
//! prints the latency and memory evidence recorded under `benchmarks/`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use siderite_cache::{Admission, AdmitMarker, Cache, CacheError, MemoryCache, RouteCache};
use siderite_core::{App, Cached, get};
use siderite_testkit::TestClient;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type CacheResult<T> = Result<T, CacheError>;
const HOUR: Duration = Duration::from_secs(3600);

/// Counts of live keys by role.
#[derive(Debug, Default, PartialEq, Eq)]
struct Census {
    markers: usize,
    counters: usize,
    epochs: usize,
    responses: usize,
}

impl Census {
    fn add(&mut self, key: &str) {
        let role = |prefix| key.starts_with(prefix);
        if role("route-v2-target:") {
            self.markers += 1;
        } else if role("route-v2-budget:") {
            self.counters += 1;
        } else if role("route-v2-epoch:") {
            self.epochs += 1;
        } else if role("route-v2-response:") {
            self.responses += 1;
        }
    }
}

/// Forwards to `MemoryCache` and remembers every key written, so the live
/// set can be counted (the cache itself has no key listing).
#[derive(Clone)]
struct Tracked {
    inner: MemoryCache,
    written: Arc<Mutex<BTreeSet<String>>>,
}

impl Tracked {
    fn new(capacity: usize) -> Self {
        Self {
            inner: MemoryCache::new(capacity),
            written: Arc::default(),
        }
    }

    fn note(&self, key: &str) {
        self.written.lock().unwrap().insert(key.to_owned());
    }

    async fn census(&self) -> Census {
        let keys: Vec<String> = self.written.lock().unwrap().iter().cloned().collect();
        let mut census = Census::default();
        for key in keys {
            if self.inner.get(&key).await.unwrap().is_some() {
                census.add(&key);
            }
        }
        census
    }

    fn stored_bytes(&self) -> usize {
        self.inner.stored_bytes()
    }

    /// Window index on the clock the adapter's counter uses: the wall
    /// clock for the generic path.
    fn window(&self, ttl: Duration) -> u64 {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        u64::try_from(now.as_millis() / ttl.as_millis()).unwrap()
    }
}

#[async_trait]
impl Cache for Tracked {
    async fn get(&self, key: &str) -> CacheResult<Option<Vec<u8>>> {
        self.inner.get(key).await
    }

    async fn set(&self, k: &str, v: Vec<u8>, t: Option<Duration>) -> CacheResult<()> {
        self.note(k);
        self.inner.set(k, v, t).await
    }

    async fn set_if_absent(&self, k: &str, v: Vec<u8>) -> CacheResult<bool> {
        self.note(k);
        self.inner.set_if_absent(k, v).await
    }

    async fn set_if_absent_with_ttl(
        &self,
        key: &str,
        v: Vec<u8>,
        ttl: Duration,
    ) -> CacheResult<bool> {
        self.note(key);
        self.inner.set_if_absent_with_ttl(key, v, ttl).await
    }

    async fn delete(&self, key: &str) -> CacheResult<bool> {
        self.inner.delete(key).await
    }

    async fn increment(&self, key: &str, by: i64) -> CacheResult<i64> {
        self.note(key);
        self.inner.increment(key, by).await
    }

    async fn increment_with_expiry(&self, key: &str, by: i64, ttl: Duration) -> CacheResult<i64> {
        self.note(key);
        self.inner.increment_with_expiry(key, by, ttl).await
    }

    async fn clear(&self) -> CacheResult<()> {
        self.inner.clear().await
    }
}

/// `MemoryCache` with its own atomic admission and rotation (the real
/// production path), recording the keys it touches. Its windows are
/// relative to its creation.
#[derive(Clone)]
struct Native {
    tracked: Tracked,
    created: Instant,
}

impl Native {
    fn new(capacity: usize) -> Self {
        Self {
            tracked: Tracked::new(capacity),
            created: Instant::now(),
        }
    }

    fn window(&self, ttl: Duration) -> u64 {
        let elapsed = self.created.elapsed();
        u64::try_from(elapsed.as_millis() / ttl.as_millis()).unwrap()
    }
}

#[async_trait]
impl Cache for Native {
    async fn get(&self, key: &str) -> CacheResult<Option<Vec<u8>>> {
        self.tracked.get(key).await
    }

    async fn set(&self, k: &str, v: Vec<u8>, t: Option<Duration>) -> CacheResult<()> {
        self.tracked.set(k, v, t).await
    }

    async fn set_if_absent_with_ttl(
        &self,
        key: &str,
        v: Vec<u8>,
        ttl: Duration,
    ) -> CacheResult<bool> {
        self.tracked.set_if_absent_with_ttl(key, v, ttl).await
    }

    async fn delete(&self, key: &str) -> CacheResult<bool> {
        self.tracked.delete(key).await
    }

    async fn increment(&self, key: &str, by: i64) -> CacheResult<i64> {
        self.tracked.increment(key, by).await
    }

    async fn increment_with_expiry(&self, key: &str, by: i64, ttl: Duration) -> CacheResult<i64> {
        self.tracked.increment_with_expiry(key, by, ttl).await
    }

    async fn clear(&self) -> CacheResult<()> {
        self.tracked.clear().await
    }

    async fn admit_marker(&self, req: &AdmitMarker<'_>) -> CacheResult<Admission> {
        // The counter key embeds a window index this adapter cannot see;
        // note the plausible ones (the census checks liveness).
        let estimate = self.window(req.window);
        for index in estimate.saturating_sub(1)..=estimate + 1 {
            self.tracked.note(&format!("{}{index}", req.budget_prefix));
        }
        self.tracked.note(req.marker);
        self.tracked.inner.admit_marker(req).await
    }

    async fn invalidate_marker(
        &self,
        marker: &str,
        epoch_key: &str,
        epoch: &[u8],
        ttl: Duration,
    ) -> CacheResult<()> {
        self.tracked.note(epoch_key);
        let inner = &self.tracked.inner;
        inner.invalidate_marker(marker, epoch_key, epoch, ttl).await
    }
}

fn client<C: Cache + Clone>(cache: C, ns: &str, budget: u64, ttl: Duration) -> TestClient {
    let layer = RouteCache::new(cache)
        .namespace(ns)
        .max_new_targets(budget)
        .generation_ttl(ttl);
    let app = App::new()
        .route(
            "/pub",
            get(|| async { Cached::public(Duration::from_secs(60), "pub") }),
        )
        .route("/priv", get(|| async { "priv" }))
        .layer(layer);
    TestClient::try_new(app).expect("app")
}

async fn status(client: &TestClient, path: &str) -> String {
    let response = client.get(path).await.unwrap();
    assert_eq!(response.status, 200);
    let value = response.headers.get("x-cache").unwrap();
    value.to_str().unwrap().to_owned()
}

/// Epoch keys are bounded per namespace by the stripe count.
const STRIPES: usize = 64;

/// Sustained distinct cacheable targets stop at the budget; targets that
/// were admitted keep hitting and the rest are served as misses.
async fn distinct_targets<C: Cache + Clone>(cache: C, probe: &Probe) {
    let (budget, total) = (50u64, 400usize);
    let client = client(cache, "distinct", budget, HOUR);
    let first = probe.window(HOUR).await;
    let mut hits = 0;
    for n in 0..total {
        status(&client, &format!("/pub?n={n}")).await;
    }
    for n in 0..total {
        hits += usize::from(status(&client, &format!("/pub?n={n}")).await == "hit");
    }
    let same_window = first == probe.window(HOUR).await;
    let census = probe.census().await;
    let budget = usize::try_from(budget).unwrap();
    if same_window {
        // One window: exactly the budget, never one more.
        assert_eq!(census.markers, budget, "{census:?}");
        assert_eq!(census.counters, 1, "{census:?}");
    } else {
        assert!(census.markers >= budget && census.markers <= 2 * budget);
        assert!(census.counters <= 2, "{census:?}");
    }
    assert!(census.epochs <= STRIPES, "stripes bound epochs: {census:?}");
    assert_eq!(census.responses, census.markers, "{census:?}");
    assert_eq!(hits, census.markers, "only admitted targets hit");
}

/// Uncacheable responses create nothing and never consume budget, so
/// cacheable targets are not starved afterwards.
async fn uncacheable<C: Cache + Clone>(cache: C, probe: &Probe) {
    let client = client(cache, "uncacheable", 10, HOUR);
    for n in 0..300 {
        assert_eq!(status(&client, &format!("/priv?n={n}")).await, "miss");
    }
    assert_eq!(probe.census().await.markers, 0);
    assert_eq!(probe.census().await.counters, 0);
    for n in 0..10 {
        status(&client, &format!("/pub?n={n}")).await;
    }
    for n in 0..10 {
        assert_eq!(status(&client, &format!("/pub?n={n}")).await, "hit");
    }
    assert_eq!(probe.census().await.markers, 10);
}

/// Each namespace has its own budget: markers grow with live namespaces
/// but stay at `namespaces x budget`.
async fn namespace_churn<C: Cache + Clone>(cache: C, probe: &Probe) {
    let (namespaces, budget) = (8usize, 10u64);
    for ns in 0..namespaces {
        let client = client(cache.clone(), &format!("churn-{ns}"), budget, HOUR);
        for n in 0..100 {
            status(&client, &format!("/pub?n={n}")).await;
        }
    }
    let census = probe.census().await;
    let budget = usize::try_from(budget).unwrap();
    assert!(census.markers >= namespaces * budget, "{census:?}");
    assert!(census.markers <= 2 * namespaces * budget, "{census:?}");
    assert!(census.counters <= 2 * namespaces, "{census:?}");
}

/// Concurrent first requests cannot overshoot the budget.
async fn concurrent_burst<C: Cache + Clone>(cache: C, probe: &Probe) {
    let client = client(cache, "burst", 20, HOUR);
    let first = probe.window(HOUR).await;
    let mut tasks = Vec::new();
    for n in 0..200 {
        let client = client.clone();
        tasks.push(tokio::spawn(async move {
            status(&client, &format!("/pub?n={n}")).await;
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let same_window = first == probe.window(HOUR).await;
    let census = probe.census().await;
    if same_window {
        assert_eq!(census.markers, 20, "{census:?}");
    } else {
        assert!(census.markers >= 20 && census.markers <= 40, "{census:?}");
    }
}

/// Expiry rolls the window: live markers never exceed two budgets while
/// distinct targets keep arriving across several windows. The census runs
/// after every request, so the peak is the true peak.
async fn window_rollover<C: Cache + Clone>(cache: C, probe: &Probe) {
    let ttl = Duration::from_millis(250);
    let client = client(cache, "rollover", 10, ttl);
    let started = Instant::now();
    let mut n = 0;
    let mut peak = 0;
    while started.elapsed() < Duration::from_millis(1500) {
        status(&client, &format!("/pub?n={n}")).await;
        n += 1;
        peak = peak.max(probe.census().await.markers);
        if n % 20 == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    assert!((10..=20).contains(&peak), "peak markers {peak}");
}

/// How to count live keys.
enum Probe {
    Generic(Tracked),
    Native(Native),
    #[cfg(feature = "redis")]
    Redis(live::Scanner),
}

impl Probe {
    async fn census(&self) -> Census {
        match self {
            Self::Generic(tracked) => tracked.census().await,
            Self::Native(native) => native.tracked.census().await,
            #[cfg(feature = "redis")]
            Self::Redis(scanner) => scanner.census().await,
        }
    }

    /// The window index on the clock the budget counter uses.
    async fn window(&self, ttl: Duration) -> u64 {
        match self {
            Self::Generic(tracked) => tracked.window(ttl),
            Self::Native(native) => native.window(ttl),
            #[cfg(feature = "redis")]
            Self::Redis(scanner) => scanner.window(ttl).await,
        }
    }
}

/// Runs a scenario against both memory paths: the atomic `MemoryCache`
/// admission and the generic sequence an external adapter gets.
macro_rules! memory_case {
    ($native:ident, $generic:ident, $scenario:ident) => {
        #[tokio::test]
        async fn $native() {
            let cache = Native::new(100_000);
            $scenario(cache.clone(), &Probe::Native(cache)).await;
        }

        #[tokio::test]
        async fn $generic() {
            let cache = Tracked::new(100_000);
            $scenario(cache.clone(), &Probe::Generic(cache)).await;
        }
    };
}

memory_case!(
    memory_distinct_targets_stay_within_budget,
    generic_distinct_targets_stay_within_budget,
    distinct_targets
);
memory_case!(
    memory_uncacheable_responses_create_no_metadata,
    generic_uncacheable_responses_create_no_metadata,
    uncacheable
);
memory_case!(
    memory_namespace_churn_is_bounded_per_namespace,
    generic_namespace_churn_is_bounded_per_namespace,
    namespace_churn
);
memory_case!(
    memory_concurrent_burst_cannot_overshoot,
    generic_concurrent_burst_cannot_overshoot,
    concurrent_burst
);
memory_case!(
    memory_window_rollover_keeps_two_windows,
    generic_window_rollover_keeps_two_windows,
    window_rollover
);

#[tokio::test]
async fn zero_budget_stores_nothing_and_skips_the_backend() -> TestResult {
    let cache = Tracked::new(64);
    let client = client(cache.clone(), "zero", 0, HOUR);
    for _ in 0..3 {
        assert_eq!(status(&client, "/pub").await, "miss");
    }
    assert!(cache.written.lock().unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn exhausted_budget_still_serves_and_admitted_targets_hit() {
    let cache = Tracked::new(1_000);
    let client = client(cache, "exhaust", 2, HOUR);
    for n in 0..2 {
        assert_eq!(status(&client, &format!("/pub?n={n}")).await, "miss");
    }
    for n in 2..6 {
        assert_eq!(status(&client, &format!("/pub?n={n}")).await, "miss");
        assert_eq!(status(&client, &format!("/pub?n={n}")).await, "miss");
    }
    for n in 0..2 {
        assert_eq!(status(&client, &format!("/pub?n={n}")).await, "hit");
    }
}

/// Latency and byte accounting evidence (run with `--nocapture`).
#[tokio::test]
async fn memory_latency_and_byte_evidence() {
    let cache = Native::new(1_000_000);
    let client = client(cache.clone(), "evidence", 10_000, HOUR);
    let before = cache.tracked.stored_bytes();
    let admitted = timed(&client, 0..10_000, "/pub").await;
    let bypassed = timed(&client, 10_000..20_000, "/pub").await;
    let hits = timed(&client, 0..10_000, "/pub").await;
    let census = cache.tracked.census().await;
    let after = cache.tracked.stored_bytes();
    println!("memory budget=10000 keys={census:?}");
    println!("memory stored_bytes before={before} after={after}");
    for (name, sample) in [("admit", admitted), ("bypass", bypassed), ("hit", hits)] {
        println!("memory latency {name}: {}", summary(sample));
    }
    assert!(census.markers <= 20_000);
}

async fn timed(client: &TestClient, range: std::ops::Range<usize>, path: &str) -> Vec<Duration> {
    let mut samples = Vec::new();
    for n in range {
        let started = Instant::now();
        status(client, &format!("{path}?n={n}")).await;
        samples.push(started.elapsed());
    }
    samples
}

fn summary(mut samples: Vec<Duration>) -> String {
    samples.sort();
    let at = |q: usize| samples[samples.len() * q / 100].as_micros();
    format!("p50={}us p99={}us n={}", at(50), at(99), samples.len())
}

#[cfg(feature = "redis")]
mod live {
    use super::*;
    use siderite_backends::redis::RedisStore;
    use siderite_cache::RedisCache;

    /// Counts keys under one test prefix with `SCAN`.
    pub struct Scanner {
        conn: redis::aio::MultiplexedConnection,
        prefix: String,
    }

    impl Scanner {
        async fn open(url: &str, prefix: &str) -> Self {
            let client = redis::Client::open(url).unwrap();
            let conn = client.get_multiplexed_async_connection().await.unwrap();
            Self {
                conn,
                prefix: prefix.to_owned(),
            }
        }

        pub async fn census(&self) -> Census {
            let mut conn = self.conn.clone();
            let pattern = format!("{}*", self.prefix);
            let mut cursor = "0".to_owned();
            let mut census = Census::default();
            loop {
                let (next, keys): (String, Vec<String>) = redis::cmd("SCAN")
                    .arg(&cursor)
                    .arg("MATCH")
                    .arg(&pattern)
                    .arg("COUNT")
                    .arg(500u32)
                    .query_async(&mut conn)
                    .await
                    .unwrap();
                for key in keys {
                    census.add(&key[self.prefix.len()..]);
                }
                if next == "0" {
                    return census;
                }
                cursor = next;
            }
        }

        /// Window index on the Redis server clock (`TIME`), the clock the
        /// admission script uses.
        pub async fn window(&self, ttl: Duration) -> u64 {
            let mut conn = self.conn.clone();
            let (secs, micros): (u64, u64) =
                redis::cmd("TIME").query_async(&mut conn).await.unwrap();
            let ms = u128::from(secs) * 1000 + u128::from(micros / 1000);
            u64::try_from(ms / ttl.as_millis()).unwrap()
        }

        async fn used_memory(&self) -> u64 {
            let mut conn = self.conn.clone();
            let info: String = redis::cmd("INFO")
                .arg("memory")
                .query_async(&mut conn)
                .await
                .unwrap();
            let line = info
                .lines()
                .find_map(|l| l.strip_prefix("used_memory:"))
                .unwrap();
            line.trim().parse().unwrap()
        }
    }

    async fn setup() -> (RedisCache, Scanner) {
        let url = std::env::var("REDIS_URL").expect("REDIS_URL");
        let store = RedisStore::connect(&url).await.unwrap();
        assert_eq!(store.database(), 15, "REDIS_URL must select database 15");
        let prefix = format!("metadata-bound-{}:", uuid::Uuid::new_v4());
        let cache = RedisCache::new(store.with_prefix(prefix.clone()));
        (cache, Scanner::open(&url, &prefix).await)
    }

    macro_rules! live_case {
        ($name:ident, $scenario:ident) => {
            #[tokio::test]
            #[ignore = "requires REDIS_URL selecting database 15"]
            async fn $name() {
                let (cache, scanner) = setup().await;
                let probe = Probe::Redis(scanner);
                $scenario(cache.clone(), &probe).await;
                cache.clear().await.unwrap();
            }
        };
    }

    live_case!(redis_distinct_targets_stay_within_budget, distinct_targets);
    live_case!(redis_uncacheable_responses_create_no_metadata, uncacheable);
    live_case!(redis_namespace_churn_is_bounded, namespace_churn);
    live_case!(redis_concurrent_burst_cannot_overshoot, concurrent_burst);
    live_case!(redis_window_rollover_keeps_two_windows, window_rollover);

    /// Latency and `used_memory` evidence (run with `--nocapture`).
    #[tokio::test]
    #[ignore = "requires REDIS_URL selecting database 15"]
    async fn redis_latency_and_memory_evidence() {
        let (cache, scanner) = setup().await;
        let client = client(cache.clone(), "evidence", 10_000, HOUR);
        let before = scanner.used_memory().await;
        let admitted = timed(&client, 0..10_000, "/pub").await;
        let bypassed = timed(&client, 10_000..20_000, "/pub").await;
        let hits = timed(&client, 0..10_000, "/pub").await;
        let census = scanner.census().await;
        let after = scanner.used_memory().await;
        println!("redis budget=10000 keys={census:?}");
        println!("redis used_memory before={before} after={after}");
        let rows = [("admit", admitted), ("bypass", bypassed), ("hit", hits)];
        for (name, sample) in rows {
            println!("redis latency {name}: {}", summary(sample));
        }
        assert!(census.markers <= 20_000);
        cache.clear().await.unwrap();
    }
}
