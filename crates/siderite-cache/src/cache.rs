//! [`Cache`] and the [`CacheExt`] JSON helpers.

use crate::CacheError;
use crate::admission::{Admission, AdmitMarker, admit_generic};
use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// Byte-oriented cache with optional per-entry time-to-live.
///
/// Keys are UTF-8 strings. Values are opaque bytes; JSON helpers live on
/// [`CacheExt`]. [`increment`](Cache::increment) follows Redis `INCRBY`: a
/// missing key is treated as `0`, and a stored value that is not a decimal
/// integer is an error.
///
/// # Contract for backends and external adapters
///
/// Only `get`, `set`, `delete`, `increment` and `clear` are required. The
/// atomic creation methods ([`set_if_absent`](Cache::set_if_absent) and
/// [`set_if_absent_with_ttl`](Cache::set_if_absent_with_ttl)) are provided
/// methods, so adding them never breaks an existing implementation. The
/// legacy `set_if_absent` default returns [`CacheError::Backend`] ("atomic
/// insertion is unsupported"); the default of `set_if_absent_with_ttl`
/// returns [`CacheError::Unsupported`]. Callers that need atomic
/// creation (RouteCache generation markers) treat that error as "bypass":
/// they serve the origin and store nothing. They never emulate creation
/// with `get` followed by `set`, which would let two creators both win and
/// let a delayed writer overwrite a newer value.
///
/// An atomic creation method must make the liveness check, the insertion and
/// the expiry deadline one indivisible step: exactly one of any set of
/// concurrent creators of a missing or expired key observes `Ok(true)`, and a
/// cancelled call either created the entry with its full TTL or created
/// nothing. See `docs/CACHE.md` for the generation-expiry contract built on
/// this primitive.
#[async_trait]
pub trait Cache: Send + Sync + 'static {
    /// Fetch `key`. `Ok(None)` when the key is missing or has expired.
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, CacheError>;

    /// Store `value` at `key`. `ttl` of `None` means the entry does not expire.
    async fn set(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>)
    -> Result<(), CacheError>;

    /// Atomically create a nonexpiring entry only when no live entry exists.
    ///
    /// Args:
    ///     key: Entry identity.
    ///     value: Opaque value to store on successful creation.
    ///
    /// Returns:
    ///     Whether this call created the entry. Backends without atomic
    ///     insertion report a capability error; RouteCache then bypasses.
    async fn set_if_absent(&self, _key: &str, _value: Vec<u8>) -> Result<bool, CacheError> {
        Err(CacheError::Backend(
            "atomic insertion is unsupported".into(),
        ))
    }

    /// Atomically create an expiring entry only when no live entry exists.
    ///
    /// The liveness check, insertion and expiry deadline are one atomic
    /// step. An existing entry that has already expired counts as absent and
    /// is replaced. A losing creator leaves the winner's value and deadline
    /// untouched.
    ///
    /// TTL granularity is one millisecond. A `ttl` that is not a whole
    /// number of milliseconds is rounded up, so a sub-millisecond TTL lives
    /// for one millisecond and an entry never expires earlier than
    /// requested. A zero `ttl` is rejected rather than treated as "already
    /// expired", because an entry that cannot be observed is never useful as
    /// a marker and several stores treat zero as "no expiry".
    ///
    /// Args:
    ///     key: Entry identity.
    ///     value: Opaque value to store on successful creation.
    ///     ttl: Positive lifetime, rounded up to whole milliseconds.
    ///
    /// Returns:
    ///     Whether this call created the entry.
    ///
    /// # Errors
    /// [`CacheError::InvalidTtl`] for a zero `ttl` or one above the portable
    /// limit (`i64::MAX / 2` milliseconds), never clamped,
    /// checked before the key is examined. [`CacheError::SizeLimit`] when
    /// the entry is refused by byte admission. [`CacheError::Unsupported`]
    /// from the default implementation: backends that cannot create and
    /// expire atomically must keep it, not emulate it.
    async fn set_if_absent_with_ttl(
        &self,
        _key: &str,
        _value: Vec<u8>,
        _ttl: Duration,
    ) -> Result<bool, CacheError> {
        Err(CacheError::Unsupported {
            operation: "set_if_absent_with_ttl",
        })
    }

    /// Remove `key`. `Ok(true)` when a live entry was removed.
    async fn delete(&self, key: &str) -> Result<bool, CacheError>;

    /// Add `by` to the integer stored at `key` (Redis `INCRBY`).
    ///
    /// A missing key starts at `0`. `by` may be negative. The stored form is
    /// the decimal representation of the new value, with no expiry when the
    /// key is created by this call; an existing key keeps its expiry.
    async fn increment(&self, key: &str, by: i64) -> Result<i64, CacheError>;

    /// Remove every entry this cache is responsible for.
    async fn clear(&self) -> Result<(), CacheError>;

    /// Atomically add `by` to a counter and give it an expiry when it has
    /// none.
    ///
    /// A missing or expired key starts at `0`. The increment and the
    /// expiry are one indivisible step: a key that exists without a
    /// deadline (including one created by this call) receives `ttl`, while
    /// a key that already has a deadline keeps it. Separate `increment` and
    /// expiry calls are not equivalent: if the key expires or is evicted
    /// between them, `INCRBY` recreates it without a TTL, a permanent
    /// counter. `ttl` follows the rules of
    /// [`set_if_absent_with_ttl`](Cache::set_if_absent_with_ttl): rounded
    /// up to whole milliseconds, zero and values above `i64::MAX / 2`
    /// milliseconds rejected.
    ///
    /// Returns:
    ///     The counter value after the increment.
    ///
    /// # Errors
    /// [`CacheError::InvalidTtl`], [`CacheError::NotInteger`], and
    /// [`CacheError::Unsupported`] from the default implementation:
    /// backends that cannot do this atomically must keep it. RouteCache
    /// uses it for its admission budget and, when unsupported, bypasses.
    async fn increment_with_expiry(
        &self,
        _key: &str,
        _by: i64,
        _ttl: Duration,
    ) -> Result<i64, CacheError> {
        Err(CacheError::Unsupported {
            operation: "increment_with_expiry",
        })
    }

    /// Admit a route-cache generation marker (provided; overriding is an
    /// optimisation, never a requirement).
    ///
    /// The default runs the generic sequence from the primitives above:
    /// create the marker with [`set_if_absent_with_ttl`], count it with
    /// [`increment_with_expiry`] only when this call created it, delete it
    /// again when over budget, then compare the epoch. An adapter lacking
    /// either primitive therefore fails with [`CacheError::Unsupported`]
    /// before any budget is spent and without leaving a marker behind.
    /// Backends that can do the whole thing atomically (the bundled ones)
    /// override it: one lock for [`MemoryCache`](crate::MemoryCache), one
    /// server-side script for Redis, whose window follows the server
    /// clock. See `docs/CACHE.md`.
    ///
    /// # Errors
    /// Backend failures, and [`CacheError::Unsupported`] from the defaults
    /// of the primitives.
    ///
    /// [`set_if_absent_with_ttl`]: Cache::set_if_absent_with_ttl
    /// [`increment_with_expiry`]: Cache::increment_with_expiry
    async fn admit_marker(&self, req: &AdmitMarker<'_>) -> Result<Admission, CacheError> {
        admit_generic(self, req).await
    }

    /// Replace the invalidation epoch and delete a marker (provided).
    ///
    /// The default does `set` with `ttl`, then `delete`. Backends override
    /// it to make the pair one atomic backend call (the bundled ones do).
    ///
    /// # Errors
    /// Backend failures.
    async fn invalidate_marker(
        &self,
        marker: &str,
        epoch_key: &str,
        epoch: &[u8],
        ttl: Duration,
    ) -> Result<(), CacheError> {
        self.set(epoch_key, epoch.to_vec(), Some(ttl)).await?;
        self.delete(marker).await.map(drop)
    }
}

#[async_trait]
impl<T: Cache + ?Sized> Cache for Arc<T> {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, CacheError> {
        (**self).get(key).await
    }

    async fn set(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<(), CacheError> {
        (**self).set(key, value, ttl).await
    }

    async fn set_if_absent(&self, key: &str, value: Vec<u8>) -> Result<bool, CacheError> {
        (**self).set_if_absent(key, value).await
    }

    async fn set_if_absent_with_ttl(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Duration,
    ) -> Result<bool, CacheError> {
        (**self).set_if_absent_with_ttl(key, value, ttl).await
    }

    async fn delete(&self, key: &str) -> Result<bool, CacheError> {
        (**self).delete(key).await
    }

    async fn increment(&self, key: &str, by: i64) -> Result<i64, CacheError> {
        (**self).increment(key, by).await
    }

    async fn clear(&self) -> Result<(), CacheError> {
        (**self).clear().await
    }

    async fn increment_with_expiry(
        &self,
        key: &str,
        by: i64,
        ttl: Duration,
    ) -> Result<i64, CacheError> {
        (**self).increment_with_expiry(key, by, ttl).await
    }

    async fn admit_marker(&self, req: &AdmitMarker<'_>) -> Result<Admission, CacheError> {
        (**self).admit_marker(req).await
    }

    async fn invalidate_marker(
        &self,
        marker: &str,
        epoch_key: &str,
        epoch: &[u8],
        ttl: Duration,
    ) -> Result<(), CacheError> {
        (**self)
            .invalidate_marker(marker, epoch_key, epoch, ttl)
            .await
    }
}

/// Largest TTL, in milliseconds, accepted by `set_if_absent_with_ttl`.
///
/// Half of `i64::MAX`: Redis rejects `PX` values whose deadline (now plus
/// the TTL) overflows a signed 64-bit millisecond clock, so the portable
/// limit leaves room for any realistic clock value. Larger TTLs are
/// rejected, never clamped.
pub(crate) const MAX_TTL_MILLIS: u64 = (i64::MAX / 2) as u64;

/// Round `ttl` up to whole milliseconds, the portable TTL granularity.
///
/// # Errors
/// [`CacheError::InvalidTtl`] for zero or when the millisecond count
/// exceeds [`MAX_TTL_MILLIS`].
pub(crate) fn ttl_millis(ttl: Duration) -> Result<u64, CacheError> {
    if ttl.is_zero() {
        return Err(CacheError::InvalidTtl);
    }
    let whole = u64::try_from(ttl.as_millis()).map_err(|_| CacheError::InvalidTtl)?;
    let ms = if ttl.subsec_nanos().is_multiple_of(1_000_000) {
        whole
    } else {
        whole.checked_add(1).ok_or(CacheError::InvalidTtl)?
    };
    if ms > MAX_TTL_MILLIS {
        return Err(CacheError::InvalidTtl);
    }
    Ok(ms)
}

/// JSON helpers implemented for every [`Cache`].
pub trait CacheExt: Cache {
    /// Fetch `key` and deserialize it as JSON of type `T`.
    ///
    /// # Errors
    /// [`CacheError::Deserialize`] when the stored bytes are not JSON of type
    /// `T`. Backend failures from [`Cache::get`].
    fn get_json<T>(&self, key: &str) -> impl Future<Output = Result<Option<T>, CacheError>> + Send
    where
        T: DeserializeOwned,
    {
        async move {
            match self.get(key).await? {
                None => Ok(None),
                Some(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|err| {
                    CacheError::Deserialize {
                        key: key.to_owned(),
                        reason: err.to_string(),
                    }
                }),
            }
        }
    }

    /// Serialize `value` as compact JSON and store it at `key`.
    ///
    /// # Errors
    /// [`CacheError::Serialize`] when `value` cannot be encoded. Backend
    /// failures from [`Cache::set`].
    fn set_json<T>(
        &self,
        key: &str,
        value: &T,
        ttl: Option<Duration>,
    ) -> impl Future<Output = Result<(), CacheError>> + Send
    where
        T: Serialize,
    {
        let encoded = serde_json::to_vec(value).map_err(|err| CacheError::Serialize {
            key: key.to_owned(),
            reason: err.to_string(),
        });
        async move { self.set(key, encoded?, ttl).await }
    }

    /// Return the JSON value at `key`, computing and storing it on a miss.
    ///
    /// A stored value that cannot be deserialized is treated as a miss so a
    /// corrupt entry can be replaced. Concurrent misses may both run `f`.
    ///
    /// # Errors
    /// Errors from `f`, JSON encoding of the computed value, or the backend.
    fn get_or_set<T, F, Fut>(
        &self,
        key: &str,
        ttl: Option<Duration>,
        f: F,
    ) -> impl Future<Output = Result<T, CacheError>> + Send
    where
        T: DeserializeOwned + Serialize + Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<T, CacheError>> + Send,
    {
        async move {
            if let Some(bytes) = self.get(key).await?
                && let Ok(value) = serde_json::from_slice::<T>(&bytes)
            {
                return Ok(value);
            }
            let value = f().await?;
            self.set_json(key, &value, ttl).await?;
            Ok(value)
        }
    }
}

impl<C: Cache> CacheExt for C {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryCache;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Widget {
        id: u32,
        name: String,
    }

    fn widget() -> Widget {
        Widget {
            id: 7,
            name: "ada".into(),
        }
    }

    #[tokio::test]
    async fn json_roundtrip_and_missing() {
        let cache = MemoryCache::new(8);
        assert_eq!(cache.get_json::<Widget>("w").await.unwrap(), None);
        cache.set_json("w", &widget(), None).await.unwrap();
        assert_eq!(cache.get_json::<Widget>("w").await.unwrap(), Some(widget()));
    }

    #[tokio::test]
    async fn get_json_rejects_invalid_payload() {
        let cache = MemoryCache::new(8);
        cache.set("w", b"not-json".to_vec(), None).await.unwrap();
        let err = cache.get_json::<Widget>("w").await.unwrap_err();
        assert!(matches!(err, CacheError::Deserialize { ref key, .. } if key == "w"));
    }

    #[tokio::test]
    async fn get_or_set_computes_once() {
        let cache = MemoryCache::new(8);
        let calls = std::sync::atomic::AtomicU32::new(0);
        let first = cache
            .get_or_set("w", None, || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Ok(widget()) }
            })
            .await
            .unwrap();
        let second = cache
            .get_or_set("w", None, || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async {
                    Ok(Widget {
                        id: 0,
                        name: "x".into(),
                    })
                }
            })
            .await
            .unwrap();
        assert_eq!(first, widget());
        assert_eq!(second, widget());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn get_or_set_does_not_store_factory_errors() {
        let cache = MemoryCache::new(8);
        let err = cache
            .get_or_set::<Widget, _, _>("w", None, || async {
                Err(CacheError::Backend("boom".into()))
            })
            .await
            .unwrap_err();
        assert!(matches!(err, CacheError::Backend(ref m) if m == "boom"));
        assert_eq!(cache.get("w").await.unwrap(), None);
    }

    #[tokio::test]
    async fn get_or_set_replaces_corrupt_entry() {
        let cache = MemoryCache::new(8);
        cache.set("w", b"nope".to_vec(), None).await.unwrap();
        let value = cache
            .get_or_set("w", None, || async { Ok(widget()) })
            .await
            .unwrap();
        assert_eq!(value, widget());
        assert_eq!(cache.get_json::<Widget>("w").await.unwrap(), Some(widget()));
    }

    /// An adapter written before the expiring creation method existed.
    struct LegacyAdapter(MemoryCache);

    #[async_trait]
    impl Cache for LegacyAdapter {
        async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, CacheError> {
            self.0.get(key).await
        }
        async fn set(
            &self,
            key: &str,
            value: Vec<u8>,
            ttl: Option<Duration>,
        ) -> Result<(), CacheError> {
            self.0.set(key, value, ttl).await
        }
        async fn delete(&self, key: &str) -> Result<bool, CacheError> {
            self.0.delete(key).await
        }
        async fn increment(&self, key: &str, by: i64) -> Result<i64, CacheError> {
            self.0.increment(key, by).await
        }
        async fn clear(&self) -> Result<(), CacheError> {
            self.0.clear().await
        }
    }

    #[tokio::test]
    async fn legacy_adapters_compile_and_report_unsupported() {
        let ttl = Duration::from_secs(1);
        let adapter = LegacyAdapter(MemoryCache::new(4));
        assert!(matches!(
            adapter.set_if_absent_with_ttl("k", vec![1], ttl).await,
            Err(CacheError::Unsupported { operation })
                if operation == "set_if_absent_with_ttl"
        ));
        assert!(adapter.set_if_absent("k", vec![1]).await.is_err());
        let shared: Arc<dyn Cache> = Arc::new(adapter);
        assert!(matches!(
            shared.set_if_absent_with_ttl("k", vec![1], ttl).await,
            Err(CacheError::Unsupported { .. })
        ));
        // Nothing was stored by the failed call.
        assert_eq!(shared.get("k").await.unwrap(), None);
    }

    #[tokio::test]
    async fn arc_forwards_expiring_creation() {
        let cache: Arc<dyn Cache> = Arc::new(MemoryCache::new(4));
        let ttl = Duration::from_secs(5);
        assert!(
            cache
                .set_if_absent_with_ttl("k", vec![1], ttl)
                .await
                .unwrap()
        );
        assert!(
            !cache
                .set_if_absent_with_ttl("k", vec![2], ttl)
                .await
                .unwrap()
        );
        assert_eq!(cache.get("k").await.unwrap(), Some(vec![1]));
    }

    #[test]
    fn ttl_millis_rounds_up_and_rejects_zero() {
        assert!(matches!(
            ttl_millis(Duration::ZERO),
            Err(CacheError::InvalidTtl)
        ));
        assert_eq!(ttl_millis(Duration::from_nanos(1)).unwrap(), 1);
        assert_eq!(ttl_millis(Duration::from_micros(999)).unwrap(), 1);
        assert_eq!(ttl_millis(Duration::from_millis(1)).unwrap(), 1);
        assert_eq!(ttl_millis(Duration::from_micros(1001)).unwrap(), 2);
        assert_eq!(ttl_millis(Duration::new(2, 1)).unwrap(), 2001);
        assert!(ttl_millis(Duration::MAX).is_err());
        let max = Duration::from_millis(MAX_TTL_MILLIS);
        assert_eq!(ttl_millis(max).unwrap(), MAX_TTL_MILLIS);
        assert!(ttl_millis(max + Duration::from_millis(1)).is_err());
        assert!(ttl_millis(max + Duration::from_nanos(1)).is_err());
    }

    #[tokio::test]
    async fn arc_dyn_cache_is_usable() {
        let cache: Arc<dyn Cache> = Arc::new(MemoryCache::new(4));
        cache.set("k", b"v".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"v"[..]));
        assert_eq!(cache.get_json::<u32>("missing").await.unwrap(), None);
    }
}
