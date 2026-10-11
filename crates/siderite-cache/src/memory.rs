//! Process-local LRU cache with per-entry TTL.

type CacheResult<T> = Result<T, CacheError>;

use crate::cache::ttl_millis;
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
            })),
        }
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
        let mut cache = self.lock();
        let now = Instant::now();
        if cache.peek(key).is_some_and(|entry| entry.is_expired(now)) {
            cache.pop(key);
        }
        let deadline;
        let next;
        if let Some(entry) = cache.get(key) {
            let invalid = || CacheError::NotInteger {
                key: key.to_owned(),
            };
            let current = parse_incr_int(&entry.value).ok_or_else(invalid)?;
            next = current
                .checked_add(by)
                .ok_or_else(|| CacheError::NotInteger {
                    key: key.to_owned(),
                })?;
            deadline = entry.expires_at;
        } else {
            next = by;
            deadline = None;
        }
        cache.put(
            key.to_owned(),
            Entry {
                value: next.to_string().into_bytes(),
                expires_at: deadline,
            },
        )?;
        Ok(next)
    }

    async fn clear(&self) -> Result<(), CacheError> {
        self.lock().clear();
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
