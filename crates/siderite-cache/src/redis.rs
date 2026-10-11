//! [`RedisCache`]: a [`Cache`] over [`siderite_backends::redis::RedisStore`].

use crate::cache::ttl_millis;
use crate::{Admission, AdmitMarker, Cache, CacheError};
use async_trait::async_trait;
use siderite_backends::redis::{MarkerAdmission, MarkerReply};
use siderite_backends::redis::{RedisError, RedisStore};
use std::time::Duration;

/// Default namespace used when wrapping a store whose prefix is empty.
const DEFAULT_PREFIX: &str = "siderite-cache:";

/// Redis-backed [`Cache`].
///
/// Wraps [`RedisStore`]. Values are stored as Redis strings. Arbitrary bytes
/// are encoded as Latin-1 (each byte becomes `U+0000..=U+00FF`) so a round
/// trip is lossless through the store's UTF-8 API. [`Cache::clear`] deletes
/// only this cache's prefix (`SCAN` + `DEL`); it never sends `FLUSHDB`.
///
/// Construct with [`RedisCache::connect`] or [`RedisCache::new`]. An empty
/// store prefix is replaced with `siderite-cache:` so [`clear`](Cache::clear)
/// cannot wipe a whole database.
#[derive(Clone)]
pub struct RedisCache {
    store: RedisStore,
}

impl std::fmt::Debug for RedisCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisCache")
            .field("store", &self.store)
            .finish()
    }
}

impl RedisCache {
    /// Wrap `store`. An empty prefix becomes `siderite-cache:`.
    #[must_use]
    pub fn new(store: RedisStore) -> Self {
        let store = if store.prefix().is_empty() {
            store.with_prefix(DEFAULT_PREFIX)
        } else {
            store
        };
        Self { store }
    }

    /// Open `url` and namespace keys under `siderite-cache:`.
    ///
    /// # Errors
    /// [`CacheError::Backend`] when the URL is rejected or Redis cannot be
    /// reached.
    pub async fn connect(url: &str) -> Result<Self, CacheError> {
        let store = RedisStore::connect(url).await?;
        Ok(Self::new(store))
    }

    /// The underlying store (already prefixed).
    #[must_use]
    pub fn store(&self) -> &RedisStore {
        &self.store
    }
}

impl From<RedisError> for CacheError {
    fn from(err: RedisError) -> Self {
        Self::Backend(err.to_string())
    }
}

/// Map each byte to `U+0000..=U+00FF` so RedisStore's UTF-8 `SET` is lossless.
fn encode_bytes(value: Vec<u8>) -> String {
    value.into_iter().map(char::from).collect()
}

fn decode_bytes(key: &str, value: String) -> Result<Vec<u8>, CacheError> {
    let mut out = Vec::with_capacity(value.len());
    for ch in value.chars() {
        match u8::try_from(u32::from(ch)) {
            Ok(byte) => out.push(byte),
            Err(_) => {
                return Err(CacheError::Backend(format!(
                    "cache key `{key}` is not a Latin-1 Redis string"
                )));
            }
        }
    }
    Ok(out)
}

fn is_incr_value_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("not an integer")
        || lower.contains("would overflow")
        || lower.contains("out of range")
}

#[async_trait]
impl Cache for RedisCache {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, CacheError> {
        match self.store.get(key).await? {
            None => Ok(None),
            Some(value) => decode_bytes(key, value).map(Some),
        }
    }

    async fn set(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<(), CacheError> {
        self.store.set(key, &encode_bytes(value), ttl).await?;
        Ok(())
    }

    async fn set_if_absent(&self, key: &str, value: Vec<u8>) -> Result<bool, CacheError> {
        Ok(self.store.set_nx(key, &encode_bytes(value)).await?)
    }

    async fn set_if_absent_with_ttl(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Duration,
    ) -> Result<bool, CacheError> {
        // One `SET key value NX PX ms`: creation and expiry are a single
        // server-side step, so a dropped future leaves no key or an
        // expiring one, never a non-expiring marker.
        let ttl = Duration::from_millis(ttl_millis(ttl)?);
        Ok(self.store.set_nx_px(key, &encode_bytes(value), ttl).await?)
    }

    async fn delete(&self, key: &str) -> Result<bool, CacheError> {
        Ok(self.store.del(key).await? > 0)
    }

    async fn increment(&self, key: &str, by: i64) -> Result<i64, CacheError> {
        match self.store.incr_by(key, by).await {
            Ok(value) => Ok(value),
            Err(RedisError::Command(message)) if is_incr_value_error(&message) => {
                Err(CacheError::NotInteger {
                    key: key.to_owned(),
                })
            }
            Err(err) => Err(err.into()),
        }
    }

    async fn increment_with_expiry(
        &self,
        key: &str,
        by: i64,
        ttl: Duration,
    ) -> Result<i64, CacheError> {
        // One server-side script: increment plus expiry-if-none.
        let ttl = Duration::from_millis(ttl_millis(ttl)?);
        match self.store.incr_by_expire(key, by, ttl).await {
            Ok(value) => Ok(value),
            Err(RedisError::Command(message)) if is_incr_value_error(&message) => {
                Err(CacheError::NotInteger {
                    key: key.to_owned(),
                })
            }
            Err(err) => Err(err.into()),
        }
    }

    async fn clear(&self) -> Result<(), CacheError> {
        self.store.delete_namespace().await?;
        Ok(())
    }

    async fn admit_marker(&self, req: &AdmitMarker<'_>) -> Result<Admission, CacheError> {
        // One server-side script (EVALSHA): epoch check, marker find or
        // create, and the budget counted in a window taken from the Redis
        // clock, so processes with skewed clocks share one window.
        let token = encode_bytes(req.token.to_vec());
        let epoch = encode_bytes(req.epoch.to_vec());
        let call = MarkerAdmission {
            marker: req.marker,
            token: &token,
            epoch_key: req.epoch_key,
            epoch: &epoch,
            budget_prefix: req.budget_prefix,
            max_new: req.max_new,
            window: req.window,
        };
        match self.store.admit_marker(call).await? {
            MarkerReply::Admitted(value) => decode_bytes(req.marker, value).map(Admission::Marker),
            MarkerReply::Stale => Ok(Admission::Stale),
            MarkerReply::Exhausted(resets_in) => Ok(Admission::Exhausted { resets_in }),
        }
    }

    async fn invalidate_marker(
        &self,
        marker: &str,
        epoch_key: &str,
        epoch: &[u8],
        ttl: Duration,
    ) -> Result<(), CacheError> {
        // `MULTI`/`EXEC`: the epoch replacement and marker delete are one
        // round trip and one atomic step.
        let epoch = encode_bytes(epoch.to_vec());
        let ttl = Duration::from_millis(ttl_millis(ttl)?);
        Ok(self
            .store
            .rotate_marker(marker, epoch_key, &epoch, ttl)
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin1_roundtrip_covers_every_byte() {
        let original: Vec<u8> = (0..=255).collect();
        let encoded = encode_bytes(original.clone());
        assert_eq!(decode_bytes("k", encoded).unwrap(), original);
    }

    #[test]
    fn decode_rejects_non_latin1() {
        let err = decode_bytes("k", "€".into()).unwrap_err();
        assert!(matches!(err, CacheError::Backend(_)));
    }

    #[test]
    fn incr_error_classifier() {
        assert!(is_incr_value_error(
            "ERR value is not an integer or out of range"
        ));
        assert!(is_incr_value_error(
            "ERR increment or decrement would overflow"
        ));
        assert!(!is_incr_value_error("WRONGTYPE Operation against a key"));
    }
}
