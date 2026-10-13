//! [`MemoryStore`]: a bounded in-process [`SessionStore`].

use crate::store::{SessionRecord, SessionStore, StoreError};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Capacity of [`MemoryStore::default`].
const DEFAULT_CAPACITY: usize = 10_000;

struct Entry {
    record: SessionRecord,
    expires: Instant,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, Entry>,
    /// Revocation epoch per user that was ever revoked.
    epochs: HashMap<String, u64>,
}

impl Inner {
    fn epoch(&self, user: &str) -> u64 {
        self.epochs.get(user).copied().unwrap_or(0)
    }

    fn stale(&self, record: &SessionRecord) -> bool {
        record
            .user
            .as_deref()
            .is_some_and(|user| record.auth_epoch != self.epoch(user))
    }

    fn purge(&mut self, now: Instant) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, entry| entry.expires > now);
        before - self.entries.len()
    }
}

/// A session store in process memory, for tests and single-process apps.
///
/// It holds at most `capacity` sessions. When it is full, expired
/// entries are dropped first; if none is expired a new session is
/// refused with a [`StoreError`] (the request fails with `503`) rather
/// than evicting somebody else's login. Run [`sweep`](Self::sweep) from a
/// scheduled job to drop expired entries between writes.
///
/// Sessions do not survive a restart and are not shared between
/// processes: use [`CacheStore`](crate::CacheStore) for that. Revoking a
/// user remembers a small epoch counter for that user for the life of the
/// process, so that a write racing the revocation cannot revive a session.
#[derive(Clone)]
pub struct MemoryStore {
    inner: Arc<Mutex<Inner>>,
    capacity: usize,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryStore")
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

impl MemoryStore {
    /// A store holding at most `capacity` sessions.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::default(),
            capacity,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Drop expired sessions and return how many were removed.
    pub fn sweep(&self) -> usize {
        self.lock().purge(Instant::now())
    }

    /// The number of stored sessions, expired ones included until swept.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// Whether no session is stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn load(
        &self,
        id_hash: &str,
    ) -> Result<Option<SessionRecord>, StoreError> {
        let now = Instant::now();
        let inner = self.lock();
        let live = inner.entries.get(id_hash).filter(|entry| {
            entry.expires > now && !inner.stale(&entry.record)
        });
        Ok(live.map(|entry| entry.record.clone()))
    }

    async fn save(
        &self,
        id_hash: &str,
        record: &SessionRecord,
        ttl: Duration,
    ) -> Result<(), StoreError> {
        let now = Instant::now();
        let mut inner = self.lock();
        if inner.stale(record) {
            return Err(StoreError::new("session was revoked"));
        }
        if !inner.entries.contains_key(id_hash)
            && inner.entries.len() >= self.capacity
        {
            inner.purge(now);
            if inner.entries.len() >= self.capacity {
                return Err(StoreError::new("session store is full"));
            }
        }
        let entry = Entry {
            record: record.clone(),
            expires: now.checked_add(ttl).unwrap_or(now),
        };
        inner.entries.insert(id_hash.to_owned(), entry);
        Ok(())
    }

    async fn delete(&self, id_hash: &str) -> Result<(), StoreError> {
        self.lock().entries.remove(id_hash);
        Ok(())
    }

    async fn revoke_user(&self, user: &str) -> Result<(), StoreError> {
        let mut inner = self.lock();
        let epoch = inner.epoch(user).saturating_add(1);
        inner.epochs.insert(user.to_owned(), epoch);
        inner
            .entries
            .retain(|_, entry| entry.record.user.as_deref() != Some(user));
        Ok(())
    }

    async fn auth_epoch(&self, user: &str) -> Result<u64, StoreError> {
        Ok(self.lock().epoch(user))
    }
}
