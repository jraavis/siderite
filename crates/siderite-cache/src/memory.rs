//! Process-local LRU cache with per-entry TTL.

type CacheResult<T> = Result<T, CacheError>;

use crate::cache::ttl_millis;
use crate::{Admission, AdmitMarker};
use crate::{Cache, CacheError};
use async_trait::async_trait;
use lru::LruCache;
use std::num::NonZeroUsize;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Hard byte-admission limits for process-local cache storage.
///
/// Key and value lengths count toward total bytes. Fixed entry/map overhead
/// is additionally bounded by entry capacity; reads clone values and can
/// temporarily allocate outside the stored-byte budget.
#[derive(Debug, Clone, Copy)]
pub struct MemoryCacheLimits {
    /// Total retained key/value bytes (default: 64 MiB).
    pub max_total_bytes: usize,
    /// Maximum bytes in one value (default: 2 MiB).
    pub max_value_bytes: usize,
    /// Maximum bytes in one key (default: 4 KiB).
    pub max_key_bytes: usize,
}

impl Default for MemoryCacheLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: 64 * 1024 * 1024,
            max_value_bytes: 2 * 1024 * 1024,
            max_key_bytes: 4096,
        }
    }
}

struct Store {
    entries: LruCache<String, Entry>,
    bytes: usize,
    limits: MemoryCacheLimits,
    /// Origin of the budget windows (this cache's own clock).
    started: Instant,
}

impl Deref for Store {
    type Target = LruCache<String, Entry>;
    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl DerefMut for Store {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entries
    }
}

impl Store {
    /// The live value at `key`, refreshing its LRU position.
    fn live(&mut self, key: &str, now: Instant) -> Option<&[u8]> {
        if self.entries.peek(key).is_some_and(|e| e.is_expired(now)) {
            self.pop(key);
            return None;
        }
        self.entries.get(key).map(|entry| entry.value.as_slice())
    }

    fn pop(&mut self, key: &str) -> Option<Entry> {
        let (key, entry) = self.entries.pop_entry(key)?;
        self.bytes -= key.len() + entry.value.len();
        Some(entry)
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    fn put(&mut self, key: String, entry: Entry) -> Result<(), CacheError> {
        let limits = self.limits;
        let size = key
            .len()
            .checked_add(entry.value.len())
            .ok_or(CacheError::SizeLimit)?;
        if key.len() > limits.max_key_bytes
            || entry.value.len() > limits.max_value_bytes
            || size > limits.max_total_bytes
        {
            return Err(CacheError::SizeLimit);
        }
        self.pop(&key);
        while self.bytes > limits.max_total_bytes - size
            || self.entries.len() >= self.entries.cap().get()
        {
            let Some((key, entry)) = self.entries.pop_lru() else {
                break;
            };
            self.bytes -= key.len() + entry.value.len();
        }
        self.bytes += size;
        self.entries.put(key, entry);
        Ok(())
    }
}

struct Entry {
    value: Vec<u8>,
    expires_at: Option<Instant>,
}

impl Entry {
    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|deadline| deadline <= now)
    }
}

/// In-memory LRU cache with per-entry time-to-live.
///
/// Cloning shares the same map. A poisoned mutex is recovered (`into_inner`)
/// so a panic in one caller does not disable the cache. `capacity` of `0` is
/// treated as `1`. Retained key/value bytes are bounded by
/// [`MemoryCacheLimits`]; writes beyond admission limits fail explicitly.
/// Expiry is lazy on access; pressure evicts LRU entries without full scans.
///
/// [`set_if_absent_with_ttl`](Cache::set_if_absent_with_ttl) is atomic: the
/// check, deadline and insertion happen under one lock acquisition. TTLs
/// round up to whole milliseconds and zero is [`CacheError::InvalidTtl`].
///
/// [`increment`](Cache::increment) matches Redis `INCRBY`: a missing or
/// expired key starts at `0`, the stored form is a decimal integer, and a
/// non-integer value is [`CacheError::NotInteger`].
#[derive(Clone)]
pub struct MemoryCache {
    inner: Arc<Mutex<Store>>,
}

impl std::fmt::Debug for MemoryCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("MemoryCache")
            .field("len", &inner.len())
            .field("capacity", &inner.cap())
            .field("stored_bytes", &inner.bytes)
            .field("byte_limits", &inner.limits)
            .finish()
    }
}

impl MemoryCache {
    /// Bound the cache to at most `capacity` live entries (minimum 1).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self::build(capacity, MemoryCacheLimits::default())
    }

    /// Configure entry count and retained key/value byte budgets.
    ///
    /// Args:
    ///     n: Maximum entry count, with zero treated as one.
    ///     lim: Positive key/value/total byte limits.
    ///
    /// Returns:
    ///     Cache with size-aware LRU eviction and hard write admission.
    ///
    /// # Errors
    /// Invalid zero limits or key/value maxima above the total budget.
    pub fn with_limits(n: usize, lim: MemoryCacheLimits) -> CacheResult<Self> {
        if lim.max_total_bytes == 0
            || lim.max_key_bytes == 0
            || lim.max_value_bytes == 0
            || lim.max_key_bytes > lim.max_total_bytes
            || lim.max_value_bytes > lim.max_total_bytes
        {
            return Err(CacheError::InvalidLimits);
        }
        Ok(Self::build(n, lim))
    }

    fn build(capacity: usize, limits: MemoryCacheLimits) -> Self {
        let size = NonZeroUsize::new(capacity.max(1));
        let cap = size.unwrap_or(NonZeroUsize::MIN);
        Self {
            inner: Arc::new(Mutex::new(Store {
                entries: LruCache::new(cap),
                bytes: 0,
                limits,
                started: Instant::now(),
            })),
        }
    }

    /// Retained key and value bytes currently counted against
    /// [`MemoryCacheLimits::max_total_bytes`].
    ///
    /// Expired entries still count until they are touched or evicted.
    #[must_use]
    pub fn stored_bytes(&self) -> usize {
        self.lock().bytes
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Store> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn expires_at(ttl: Option<Duration>) -> Result<Option<Instant>, CacheError> {
    match ttl {
        Some(ttl) => Instant::now()
            .checked_add(ttl)
            .map(Some)
            .ok_or(CacheError::InvalidTtl),
        None => Ok(None),
    }
}

/// Redis `INCRBY` accepts an optional ASCII minus and one or more digits.
fn parse_incr_int(bytes: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(bytes).ok()?;
    if text.is_empty() {
        return None;
    }
    let digits = match text.as_bytes()[0] {
        b'-' => &text.as_bytes()[1..],
        b'0'..=b'9' => text.as_bytes(),
        _ => return None,
    };
    if digits.is_empty() || !digits.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

impl MemoryCache {
    /// Increment under one lock; `expiry` applies only to a key that has
    /// no deadline (new, or live without one).
    fn incr_locked(&self, key: &str, by: i64, expiry: Option<Duration>) -> CacheResult<i64> {
        let mut cache = self.lock();
        let now = Instant::now();
        if cache.peek(key).is_some_and(|entry| entry.is_expired(now)) {
            cache.pop(key);
        }
        let invalid = || CacheError::NotInteger {
            key: key.to_owned(),
        };
        let (next, deadline) = match cache.get(key) {
            Some(entry) => {
                let current = parse_incr_int(&entry.value).ok_or_else(invalid)?;
                let next = current.checked_add(by).ok_or_else(invalid)?;
                (next, entry.expires_at)
            }
            None => (by, None),
        };
        let deadline = match (deadline, expiry) {
            (None, Some(ttl)) => Some(now.checked_add(ttl).ok_or(CacheError::InvalidTtl)?),
            (existing, _) => existing,
        };
        cache.put(
            key.to_owned(),
            Entry {
                value: next.to_string().into_bytes(),
                expires_at: deadline,
            },
        )?;
        Ok(next)
    }
}

#[async_trait]
impl Cache for MemoryCache {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, CacheError> {
        let mut cache = self.lock();
        let now = Instant::now();
        let expired = cache.peek(key).is_some_and(|e| e.is_expired(now));
        if expired {
            cache.pop(key);
            return Ok(None);
        }
        Ok(cache.get(key).map(|entry| entry.value.clone()))
    }

    async fn set(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<(), CacheError> {
        let mut cache = self.lock();
        cache.put(
            key.to_owned(),
            Entry {
                value,
                expires_at: expires_at(ttl)?,
            },
        )
    }

    async fn set_if_absent(&self, key: &str, v: Vec<u8>) -> CacheResult<bool> {
        let mut cache = self.lock();
        let now = Instant::now();
        if cache.peek(key).is_some_and(|entry| !entry.is_expired(now)) {
            return Ok(false);
        }
        cache.put(
            key.to_owned(),
            Entry {
                value: v,
                expires_at: None,
            },
        )?;
        Ok(true)
    }

    async fn set_if_absent_with_ttl(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Duration,
    ) -> CacheResult<bool> {
        let ttl = Duration::from_millis(ttl_millis(ttl)?);
        // One lock section: liveness check, deadline and insertion share a
        // single clock reading, so no creator can observe a half-state.
        let mut cache = self.lock();
        let now = Instant::now();
        if cache.peek(key).is_some_and(|entry| !entry.is_expired(now)) {
            return Ok(false);
        }
        let deadline = now.checked_add(ttl).ok_or(CacheError::InvalidTtl)?;
        cache.put(
            key.to_owned(),
            Entry {
                value,
                expires_at: Some(deadline),
            },
        )?;
        Ok(true)
    }

    async fn delete(&self, key: &str) -> Result<bool, CacheError> {
        let mut cache = self.lock();
        let now = Instant::now();
        match cache.peek(key) {
            Some(entry) if entry.is_expired(now) => {
                cache.pop(key);
                Ok(false)
            }
            Some(_) => {
                cache.pop(key);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn increment(&self, key: &str, by: i64) -> Result<i64, CacheError> {
        self.incr_locked(key, by, None)
    }

    async fn increment_with_expiry(
        &self,
        key: &str,
        by: i64,
        ttl: Duration,
    ) -> Result<i64, CacheError> {
        let ttl = Duration::from_millis(ttl_millis(ttl)?);
        self.incr_locked(key, by, Some(ttl))
    }

    async fn clear(&self) -> Result<(), CacheError> {
        self.lock().clear();
        Ok(())
    }

    async fn admit_marker(&self, req: &AdmitMarker<'_>) -> CacheResult<Admission> {
        let window = Duration::from_millis(ttl_millis(req.window)?);
        let counter_ttl = window.checked_mul(2).ok_or(CacheError::InvalidTtl)?;
        let mut cache = self.lock();
        let now = Instant::now();
        let epoch = cache.live(req.epoch_key, now);
        if epoch.is_none_or(|current| current != req.epoch) {
            return Ok(Admission::Stale);
        }
        if let Some(live) = cache.live(req.marker, now) {
            return Ok(Admission::Marker(live.to_vec()));
        }
        // The window comes from this cache's own clock (one process).
        let ms = window.as_millis().max(1);
        let elapsed = now.saturating_duration_since(cache.started).as_millis();
        let counter = format!("{}{}", req.budget_prefix, elapsed / ms);
        let used = match cache.live(&counter, now) {
            Some(value) => parse_incr_int(value).ok_or_else(|| CacheError::NotInteger {
                key: counter.clone(),
            })?,
            None => 0,
        };
        let used = u64::try_from(used).unwrap_or(u64::MAX);
        if used >= req.max_new {
            let left = u64::try_from(ms - elapsed % ms).unwrap_or(u64::MAX);
            let resets_in = Duration::from_millis(left);
            return Ok(Admission::Exhausted { resets_in });
        }
        let counted = Entry {
            value: (used + 1).to_string().into_bytes(),
            expires_at: Some(now.checked_add(counter_ttl).ok_or(CacheError::InvalidTtl)?),
        };
        // Keep the live counter's deadline: only a new counter gets one.
        let counted = match cache.peek(&counter) {
            Some(old) if !old.is_expired(now) => Entry {
                expires_at: old.expires_at,
                ..counted
            },
            _ => counted,
        };
        let deadline = now.checked_add(window).ok_or(CacheError::InvalidTtl)?;
        let marker = Entry {
            value: req.token.to_vec(),
            expires_at: Some(deadline),
        };
        // Marker first: a refused marker must not consume budget.
        cache.put(req.marker.to_owned(), marker)?;
        if let Err(error) = cache.put(counter, counted) {
            cache.pop(req.marker);
            return Err(error);
        }
        Ok(Admission::Marker(req.token.to_vec()))
    }

    async fn invalidate_marker(
        &self,
        marker: &str,
        epoch_key: &str,
        epoch: &[u8],
        ttl: Duration,
    ) -> CacheResult<()> {
        let ttl = Duration::from_millis(ttl_millis(ttl)?);
        let mut cache = self.lock();
        let deadline = Instant::now().checked_add(ttl);
        let deadline = deadline.ok_or(CacheError::InvalidTtl)?;
        let entry = Entry {
            value: epoch.to_vec(),
            expires_at: Some(deadline),
        };
        // Marker first: a refused epoch put cannot leave the marker.
        cache.pop(marker);
        cache.put(epoch_key.to_owned(), entry)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CacheExt;
    use std::thread;
    use std::time::Duration;

    #[tokio::test]
    async fn get_set_delete_clear() {
        let cache = MemoryCache::new(8);
        assert_eq!(cache.get("k").await.unwrap(), None);
        cache.set("k", b"v".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"v"[..]));
        assert!(cache.delete("k").await.unwrap());
        assert!(!cache.delete("k").await.unwrap());
        cache.set("a", b"1".to_vec(), None).await.unwrap();
        cache.set("b", b"2".to_vec(), None).await.unwrap();
        cache.clear().await.unwrap();
        assert_eq!(cache.get("a").await.unwrap(), None);
        assert_eq!(cache.get("b").await.unwrap(), None);
    }

    #[tokio::test]
    async fn increment_with_expiry_sets_a_deadline_once() {
        let cache = MemoryCache::new(8);
        let ttl = Duration::from_millis(60);
        assert_eq!(cache.increment_with_expiry("c", 1, ttl).await.unwrap(), 1);
        thread::sleep(Duration::from_millis(40));
        // A later increment keeps the original deadline.
        let long = Duration::from_secs(60);
        assert_eq!(cache.increment_with_expiry("c", 2, long).await.unwrap(), 3);
        thread::sleep(Duration::from_millis(40));
        assert_eq!(cache.get("c").await.unwrap(), None);
        // After expiry the counter restarts at zero with a fresh deadline.
        assert_eq!(cache.increment_with_expiry("c", 1, long).await.unwrap(), 1);
        // A key without a deadline receives one.
        cache.set("p", b"5".to_vec(), None).await.unwrap();
        assert_eq!(cache.increment_with_expiry("p", 1, ttl).await.unwrap(), 6);
        thread::sleep(Duration::from_millis(80));
        assert_eq!(cache.get("p").await.unwrap(), None);
    }

    #[tokio::test]
    async fn increment_with_expiry_validates_ttl_and_value() {
        let cache = MemoryCache::new(8);
        let zero = cache.increment_with_expiry("c", 1, Duration::ZERO).await;
        assert!(matches!(zero, Err(CacheError::InvalidTtl)));
        assert_eq!(cache.get("c").await.unwrap(), None);
        cache.set("s", b"x".to_vec(), None).await.unwrap();
        let bad = cache.increment_with_expiry("s", 1, Duration::from_secs(1));
        assert!(matches!(bad.await, Err(CacheError::NotInteger { .. })));
    }

    fn request<'a>(marker: &'a str, token: &'a [u8], epoch: &'a [u8]) -> AdmitMarker<'a> {
        let window = Duration::from_millis(80);
        AdmitMarker::new(marker, token, "epoch", epoch).budget("budget:", 2, window)
    }

    #[tokio::test]
    async fn admit_marker_checks_epoch_counts_creations_and_rolls_windows() {
        let cache = MemoryCache::new(64);
        let epoch: &[u8] = &[7];
        cache.set("epoch", epoch.to_vec(), None).await.unwrap();
        let stale = cache.admit_marker(&request("a", &[1], &[8])).await;
        assert_eq!(stale.unwrap(), Admission::Stale);
        // A missing epoch counts as changed.
        let gone = AdmitMarker::new("a", &[1], "elsewhere", epoch);
        assert_eq!(cache.admit_marker(&gone).await.unwrap(), Admission::Stale);
        assert_eq!(cache.get("a").await.unwrap(), None, "stale creates nothing");
        let a = cache
            .admit_marker(&request("a", &[1], epoch))
            .await
            .unwrap();
        assert_eq!(a, Admission::Marker(vec![1]));
        // An existing marker is returned and costs no budget.
        let again = cache
            .admit_marker(&request("a", &[9], epoch))
            .await
            .unwrap();
        assert_eq!(again, Admission::Marker(vec![1]));
        let b = cache
            .admit_marker(&request("b", &[2], epoch))
            .await
            .unwrap();
        assert_eq!(b, Admission::Marker(vec![2]));
        let over = cache
            .admit_marker(&request("c", &[3], epoch))
            .await
            .unwrap();
        let Admission::Exhausted { resets_in } = over else {
            panic!("expected exhaustion, got {over:?}");
        };
        assert!(resets_in <= Duration::from_millis(80));
        assert_eq!(cache.get("c").await.unwrap(), None, "no marker over budget");
        // The next window has a fresh budget.
        thread::sleep(Duration::from_millis(90));
        let c = cache
            .admit_marker(&request("c", &[3], epoch))
            .await
            .unwrap();
        assert_eq!(c, Admission::Marker(vec![3]));
    }

    #[tokio::test]
    async fn admit_marker_is_exact_under_concurrency() {
        let cache = MemoryCache::new(1024);
        cache.set("epoch", vec![7], None).await.unwrap();
        let window = Duration::from_secs(3600);
        let mut tasks = Vec::new();
        for n in 0..100u8 {
            let cache = cache.clone();
            tasks.push(tokio::spawn(async move {
                let marker = format!("m{n}");
                let token = [n];
                let base = AdmitMarker::new(&marker, &token, "epoch", &[7]);
                let request = base.budget("budget:", 10, window);
                cache.admit_marker(&request).await.unwrap()
            }));
        }
        let mut admitted = 0;
        for task in tasks {
            admitted += usize::from(matches!(task.await.unwrap(), Admission::Marker(_)));
        }
        assert_eq!(admitted, 10);
    }

    #[tokio::test]
    async fn invalidate_marker_replaces_epoch_and_deletes_marker() {
        let cache = MemoryCache::new(8);
        cache.set("marker", vec![1], None).await.unwrap();
        let ttl = Duration::from_millis(40);
        cache
            .invalidate_marker("marker", "epoch", &[5], ttl)
            .await
            .unwrap();
        assert_eq!(cache.get("marker").await.unwrap(), None);
        assert_eq!(cache.get("epoch").await.unwrap(), Some(vec![5]));
        thread::sleep(Duration::from_millis(60));
        assert_eq!(cache.get("epoch").await.unwrap(), None, "epoch expires");
        let zero = cache
            .invalidate_marker("m", "e", &[1], Duration::ZERO)
            .await;
        assert!(matches!(zero, Err(CacheError::InvalidTtl)));
    }

    #[tokio::test]
    async fn stored_bytes_tracks_keys_and_values() {
        let cache = MemoryCache::new(8);
        assert_eq!(cache.stored_bytes(), 0);
        cache.set("key", b"value".to_vec(), None).await.unwrap();
        assert_eq!(cache.stored_bytes(), 8);
        cache.delete("key").await.unwrap();
        assert_eq!(cache.stored_bytes(), 0);
    }

    #[tokio::test]
    async fn clones_share_storage() {
        let cache = MemoryCache::new(4);
        let clone = cache.clone();
        clone.set("k", b"v".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"v"[..]));
    }

    #[tokio::test]
    async fn expired_entries_are_missing() {
        let cache = MemoryCache::new(4);
        cache
            .set("k", b"v".to_vec(), Some(Duration::from_millis(15)))
            .await
            .unwrap();
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"v"[..]));
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(cache.get("k").await.unwrap(), None);
        assert!(!cache.delete("k").await.unwrap());
    }

    #[tokio::test]
    async fn lru_evicts_the_least_recently_used() {
        let cache = MemoryCache::new(2);
        cache.set("a", b"1".to_vec(), None).await.unwrap();
        cache.set("b", b"2".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("a").await.unwrap().as_deref(), Some(&b"1"[..]));
        cache.set("c", b"3".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("b").await.unwrap(), None);
        assert_eq!(cache.get("a").await.unwrap().as_deref(), Some(&b"1"[..]));
        assert_eq!(cache.get("c").await.unwrap().as_deref(), Some(&b"3"[..]));
    }

    #[tokio::test]
    async fn zero_capacity_is_one() {
        let cache = MemoryCache::new(0);
        cache.set("a", b"1".to_vec(), None).await.unwrap();
        cache.set("b", b"2".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("a").await.unwrap(), None);
        assert_eq!(cache.get("b").await.unwrap().as_deref(), Some(&b"2"[..]));
    }

    #[tokio::test]
    async fn increment_matches_redis_incrby() {
        let cache = MemoryCache::new(8);
        assert_eq!(cache.increment("n", 3).await.unwrap(), 3);
        assert_eq!(cache.increment("n", 2).await.unwrap(), 5);
        assert_eq!(cache.increment("n", -8).await.unwrap(), -3);
        assert_eq!(cache.get("n").await.unwrap().as_deref(), Some(&b"-3"[..]));
        cache.set("n", b"9".to_vec(), None).await.unwrap();
        assert_eq!(cache.increment("n", 1).await.unwrap(), 10);
    }

    #[tokio::test]
    async fn increment_rejects_non_integers_and_overflow() {
        let cache = MemoryCache::new(8);
        cache.set("s", b"hello".to_vec(), None).await.unwrap();
        assert!(matches!(
            cache.increment("s", 1).await.unwrap_err(),
            CacheError::NotInteger { ref key } if key == "s"
        ));
        cache.set("plus", b"+1".to_vec(), None).await.unwrap();
        assert!(matches!(
            cache.increment("plus", 1).await.unwrap_err(),
            CacheError::NotInteger { .. }
        ));
        cache.set("ws", b" 1".to_vec(), None).await.unwrap();
        assert!(matches!(
            cache.increment("ws", 1).await.unwrap_err(),
            CacheError::NotInteger { .. }
        ));
        cache
            .set("max", i64::MAX.to_string().into_bytes(), None)
            .await
            .unwrap();
        assert!(matches!(
            cache.increment("max", 1).await.unwrap_err(),
            CacheError::NotInteger { ref key } if key == "max"
        ));
    }

    #[tokio::test]
    async fn increment_on_expired_key_starts_at_zero() {
        let cache = MemoryCache::new(4);
        cache
            .set("n", b"9".to_vec(), Some(Duration::from_millis(15)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(cache.increment("n", 4).await.unwrap(), 4);
    }

    #[tokio::test]
    async fn increment_preserves_ttl_of_a_live_key() {
        let cache = MemoryCache::new(4);
        cache
            .set("n", b"1".to_vec(), Some(Duration::from_millis(80)))
            .await
            .unwrap();
        assert_eq!(cache.increment("n", 1).await.unwrap(), 2);
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(cache.get("n").await.unwrap(), None);
    }

    #[tokio::test]
    async fn concurrent_increments_are_serialized() {
        let cache = MemoryCache::new(4);
        let mut joins = Vec::new();
        for _ in 0..32 {
            let cache = cache.clone();
            let worker = async move { cache.increment("n", 1).await };
            joins.push(tokio::spawn(worker));
        }
        let mut total = 0;
        for join in joins {
            total += join.await.unwrap().unwrap();
        }
        assert_eq!(cache.get("n").await.unwrap().as_deref(), Some(&b"32"[..]));
        assert_eq!(total, (1..=32).sum::<i64>());
    }

    #[tokio::test]
    async fn poisoned_mutex_is_recovered() {
        let cache = MemoryCache::new(4);
        cache.set("k", b"v".to_vec(), None).await.unwrap();
        let poison = cache.clone();
        let _ = thread::spawn(move || {
            let _guard = poison.lock();
            panic!("poison");
        })
        .join();
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"v"[..]));
        cache.set("k", b"w".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"w"[..]));
    }

    #[tokio::test]
    async fn binary_values_roundtrip() {
        let cache = MemoryCache::new(2);
        let value = vec![0, 127, 128, 255];
        cache.set("bin", value.clone(), None).await.unwrap();
        assert_eq!(cache.get("bin").await.unwrap(), Some(value));
    }

    #[tokio::test]
    async fn set_replaces_ttl() {
        let cache = MemoryCache::new(2);
        cache
            .set("k", b"1".to_vec(), Some(Duration::from_millis(15)))
            .await
            .unwrap();
        cache.set("k", b"2".to_vec(), None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"2"[..]));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn expiring_creation_has_one_winner() {
        let cache = MemoryCache::new(8);
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(64));
        let mut joins = Vec::new();
        for v in 0..64u8 {
            let cache = cache.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            joins.push(tokio::spawn(async move {
                let ttl = Duration::from_secs(30);
                barrier.wait().await;
                cache.set_if_absent_with_ttl("k", vec![v], ttl).await
            }));
        }
        let mut winners = Vec::new();
        for (v, join) in joins.into_iter().enumerate() {
            if join.await.unwrap().unwrap() {
                winners.push(v as u8);
            }
        }
        assert_eq!(winners.len(), 1);
        assert_eq!(cache.get("k").await.unwrap(), Some(winners));
    }

    #[tokio::test]
    async fn expiring_creation_rejects_zero_before_touching_state() {
        let cache = MemoryCache::new(4);
        cache.set("k", b"live".to_vec(), None).await.unwrap();
        let err = cache.set_if_absent_with_ttl("k", vec![1], Duration::ZERO);
        assert!(matches!(err.await, Err(CacheError::InvalidTtl)));
        let err = cache.set_if_absent_with_ttl("new", vec![1], Duration::ZERO);
        assert!(matches!(err.await, Err(CacheError::InvalidTtl)));
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"live"[..]));
        assert_eq!(cache.get("new").await.unwrap(), None);
        let huge = cache.set_if_absent_with_ttl("new", vec![1], Duration::MAX);
        assert!(matches!(huge.await, Err(CacheError::InvalidTtl)));
    }

    #[tokio::test]
    async fn expiring_creation_rounds_subunit_ttl_up() {
        let cache = MemoryCache::new(4);
        let before = Instant::now();
        let tiny = Duration::from_nanos(1);
        assert!(
            cache
                .set_if_absent_with_ttl("k", vec![1], tiny)
                .await
                .unwrap()
        );
        let deadline = cache.lock().peek("k").and_then(|e| e.expires_at);
        let deadline = deadline.expect("entry has a deadline");
        assert!(deadline >= before + Duration::from_millis(1));
        let odd = Duration::from_micros(1500);
        assert!(
            cache
                .set_if_absent_with_ttl("j", vec![1], odd)
                .await
                .unwrap()
        );
        let before = Instant::now();
        let deadline = cache.lock().peek("j").and_then(|e| e.expires_at);
        assert!(deadline.expect("deadline") <= before + Duration::from_millis(2));
    }

    #[tokio::test]
    async fn expiring_creation_replaces_expired_and_expires() {
        let cache = MemoryCache::new(4);
        let ms = Duration::from_millis(5);
        assert!(
            cache
                .set_if_absent_with_ttl("k", vec![1], ms)
                .await
                .unwrap()
        );
        assert!(
            !cache
                .set_if_absent_with_ttl("k", vec![2], ms)
                .await
                .unwrap()
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
        // Expired entries are absent for creation even before a get evicts.
        let long = Duration::from_secs(30);
        assert!(
            cache
                .set_if_absent_with_ttl("k", vec![3], long)
                .await
                .unwrap()
        );
        assert_eq!(cache.get("k").await.unwrap(), Some(vec![3]));
        // The loser never extends the winner's deadline.
        let one = Duration::from_millis(5);
        assert!(
            !cache
                .set_if_absent_with_ttl("k", vec![4], one)
                .await
                .unwrap()
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(cache.get("k").await.unwrap(), Some(vec![3]));
    }

    #[tokio::test]
    async fn expiring_creation_obeys_byte_admission() {
        let lim = MemoryCacheLimits {
            max_total_bytes: 8,
            max_value_bytes: 4,
            max_key_bytes: 4,
        };
        let cache = MemoryCache::with_limits(4, lim).unwrap();
        let ttl = Duration::from_secs(1);
        let err = cache.set_if_absent_with_ttl("k", vec![0; 5], ttl).await;
        assert!(matches!(err, Err(CacheError::SizeLimit)));
        assert_eq!(cache.get("k").await.unwrap(), None);
    }

    #[test]
    fn parse_incr_int_is_strict() {
        assert_eq!(parse_incr_int(b"0"), Some(0));
        assert_eq!(parse_incr_int(b"01"), Some(1));
        assert_eq!(parse_incr_int(b"-12"), Some(-12));
        assert_eq!(parse_incr_int(b""), None);
        assert_eq!(parse_incr_int(b"-"), None);
        assert_eq!(parse_incr_int(b"+1"), None);
        assert_eq!(parse_incr_int(b"1 "), None);
        assert_eq!(parse_incr_int(b"1.0"), None);
        assert_eq!(parse_incr_int(b"abc"), None);
    }

    #[test]
    fn types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MemoryCache>();
        assert_send_sync::<Box<dyn Cache>>();
    }

    #[tokio::test]
    async fn json_helper_uses_compact_encoding() {
        let cache = MemoryCache::new(2);
        cache.set_json("n", &42u32, None).await.unwrap();
        assert_eq!(cache.get("n").await.unwrap().as_deref(), Some(&b"42"[..]));
    }
}

#[cfg(test)]
#[path = "memory_limits_tests.rs"]
mod limits_tests;
