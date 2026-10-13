//! [`CacheStore`]: sessions in any [`siderite_cache::Cache`].

use crate::id::id_hash;
use crate::store::{SessionRecord, SessionStore, StoreError};
use async_trait::async_trait;
use siderite_cache::{Cache, CacheExt};
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_PREFIX: &str = "siderite:session:";
/// How long a deleted id stays refused: longer than the default absolute
/// lifetime of 12 hours.
const DEFAULT_TOMBSTONE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Sessions stored as JSON in a [`Cache`], with the store TTL.
///
/// Revoking a user is one write: it increments a per-user epoch counter,
/// and [`load`](SessionStore::load) rejects any record whose
/// [`auth_epoch`](SessionRecord::auth_epoch) differs from the counter.
/// That works on `MemoryCache` and Redis alike and also stops a stale
/// in-flight write from reviving a revoked session.
///
/// The epoch counters have no expiry. Use a cache that does not evict
/// them: Redis with `noeviction` or a `volatile-*` policy, or a
/// `MemoryCache` sized so it never evicts (it is an LRU, and an evicted
/// counter would revive revoked sessions). Keys hold the session id hash
/// and a hash of the user name, never the raw values.
///
/// Deleting a session (logout, rotation) also writes a tombstone for its
/// id, and [`load`](SessionStore::load) refuses an id with a tombstone.
/// The cache has no conditional write, so a slow request racing a logout
/// could store the record again; the tombstone keeps that record from
/// ever being served. A hit therefore costs one more read. The tombstone
/// lifetime ([`with_tombstone_ttl`](Self::with_tombstone_ttl), 24 hours
/// by default) must be at least the session's absolute lifetime;
/// [`update`](SessionStore::update) never writes a record that outlives
/// it.
///
/// Errors from the cache are replaced by a generic [`StoreError`]: cache
/// error texts can carry addresses.
pub struct CacheStore<C: Cache> {
    cache: Arc<C>,
    prefix: String,
    tombstone_ttl: Duration,
}

impl<C: Cache> Clone for CacheStore<C> {
    fn clone(&self) -> Self {
        Self {
            cache: Arc::clone(&self.cache),
            prefix: self.prefix.clone(),
            tombstone_ttl: self.tombstone_ttl,
        }
    }
}

impl<C: Cache> std::fmt::Debug for CacheStore<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheStore")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl<C: Cache> CacheStore<C> {
    /// Store sessions in `cache` under the `siderite:session:` prefix.
    #[must_use]
    pub fn new(cache: C) -> Self {
        Self {
            cache: Arc::new(cache),
            prefix: DEFAULT_PREFIX.to_owned(),
            tombstone_ttl: DEFAULT_TOMBSTONE_TTL,
        }
    }

    /// Keep deleted ids refused for `ttl`. Set it to at least the
    /// session's [absolute lifetime](crate::SessionConfig::absolute_lifetime).
    #[must_use]
    pub fn with_tombstone_ttl(mut self, ttl: Duration) -> Self {
        self.tombstone_ttl = ttl;
        self
    }

    /// Use another key prefix, for example one per application.
    #[must_use]
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    fn record_key(&self, id_hash: &str) -> String {
        format!("{}s:{id_hash}", self.prefix)
    }

    fn tombstone_key(&self, id_hash: &str) -> String {
        format!("{}d:{id_hash}", self.prefix)
    }

    async fn deleted(&self, id_hash: &str) -> Result<bool, StoreError> {
        self.cache
            .get(&self.tombstone_key(id_hash))
            .await
            .map(|found| found.is_some())
            .map_err(|_| unavailable())
    }

    fn epoch_key(&self, user: &str) -> String {
        format!("{}e:{}", self.prefix, id_hash(user))
    }

    async fn epoch(&self, user: &str) -> Result<u64, StoreError> {
        let bytes = self
            .cache
            .get(&self.epoch_key(user))
            .await
            .map_err(|_| unavailable())?;
        let Some(bytes) = bytes else {
            return Ok(0);
        };
        std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| text.parse::<u64>().ok())
            .ok_or_else(|| StoreError::new("corrupt revocation epoch"))
    }
}

fn unavailable() -> StoreError {
    StoreError::new("cache backend unavailable")
}

#[async_trait]
impl<C: Cache> SessionStore for CacheStore<C> {
    async fn load(
        &self,
        id_hash: &str,
    ) -> Result<Option<SessionRecord>, StoreError> {
        let key = self.record_key(id_hash);
        let record = self
            .cache
            .get_json::<SessionRecord>(&key)
            .await
            .map_err(|_| unavailable())?;
        let Some(record) = record else {
            return Ok(None);
        };
        if self.deleted(id_hash).await? {
            return Ok(None);
        }
        if let Some(user) = &record.user
            && self.epoch(user).await? != record.auth_epoch
        {
            return Ok(None);
        }
        Ok(Some(record))
    }

    async fn save(
        &self,
        id_hash: &str,
        record: &SessionRecord,
        ttl: Duration,
    ) -> Result<(), StoreError> {
        let key = self.record_key(id_hash);
        self.cache
            .set_json(&key, record, Some(ttl))
            .await
            .map_err(|_| unavailable())
    }

    async fn update(
        &self,
        id_hash: &str,
        record: &SessionRecord,
        ttl: Duration,
    ) -> Result<bool, StoreError> {
        let key = self.record_key(id_hash);
        let stored = self.cache.get(&key).await.map_err(|_| unavailable())?;
        if stored.is_none() || self.deleted(id_hash).await? {
            return Ok(false);
        }
        let ttl = ttl.min(self.tombstone_ttl);
        self.cache
            .set_json(&key, record, Some(ttl))
            .await
            .map(|()| true)
            .map_err(|_| unavailable())
    }

    async fn delete(&self, id_hash: &str) -> Result<(), StoreError> {
        // The tombstone first: if removing the record then fails, the id
        // is already refused.
        let tombstone = self.tombstone_key(id_hash);
        let ttl = Some(self.tombstone_ttl);
        self.cache
            .set(&tombstone, b"1".to_vec(), ttl)
            .await
            .map_err(|_| unavailable())?;
        let key = self.record_key(id_hash);
        self.cache
            .delete(&key)
            .await
            .map(|_| ())
            .map_err(|_| unavailable())
    }

    async fn revoke_user(&self, user: &str) -> Result<(), StoreError> {
        self.cache
            .increment(&self.epoch_key(user), 1)
            .await
            .map(|_| ())
            .map_err(|_| unavailable())
    }

    async fn auth_epoch(&self, user: &str) -> Result<u64, StoreError> {
        self.epoch(user).await
    }
}
