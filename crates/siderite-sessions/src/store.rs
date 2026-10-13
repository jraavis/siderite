//! The [`SessionStore`] trait and the data it persists.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;
use std::time::Duration;

/// One stored session.
///
/// Times are seconds since the Unix epoch. The record never contains the
/// session id: stores key it by the SHA-256 hex of the id (see
/// [`id_hash`](crate::id_hash)).
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SessionRecord {
    /// The authenticated user, set by [`Session::login`](crate::Session).
    pub user: Option<String>,
    /// Application data.
    pub data: Map<String, Value>,
    /// When the session was created (login resets it).
    pub created_at: u64,
    /// When the session was last written; drives the idle timeout.
    pub last_seen: u64,
    /// The user's revocation epoch at login. A store that revokes by
    /// epoch (see [`SessionStore::revoke_user`]) rejects older records.
    pub auth_epoch: u64,
}

impl fmt::Debug for SessionRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Application data and user names stay out of logs.
        f.debug_struct("SessionRecord")
            .field("authenticated", &self.user.is_some())
            .field("keys", &self.data.len())
            .field("created_at", &self.created_at)
            .field("last_seen", &self.last_seen)
            .field("auth_epoch", &self.auth_epoch)
            .finish()
    }
}

/// A store failure.
///
/// The message must never contain addresses, credentials, session ids or
/// stored data: it can reach logs, and a failure never reaches clients.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("session store error: {0}")]
pub struct StoreError(String);

impl StoreError {
    /// Create an error with a short, secret-free `message`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Server-side storage of sessions, keyed by the SHA-256 hex of the id.
///
/// The store never sees a raw session id, so a leaked table or backup does
/// not hand out live sessions. Implementations must be safe for concurrent
/// use. Requests of one session are last-writer-wins: the layer does not
/// merge concurrent changes.
///
/// A store is not asked to enforce idle or absolute lifetimes (the layer
/// does, from the record), but it must drop entries after `ttl` so that
/// abandoned sessions disappear.
#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    /// Fetch the record for `id_hash`; `Ok(None)` when absent or expired
    /// or revoked.
    ///
    /// # Errors
    /// [`StoreError`] when the store cannot be read. The layer then
    /// answers `503` instead of treating the request as anonymous.
    async fn load(
        &self,
        id_hash: &str,
    ) -> Result<Option<SessionRecord>, StoreError>;

    /// Store `record` under `id_hash`, replacing any record, for `ttl`.
    ///
    /// # Errors
    /// [`StoreError`] when the record was not stored.
    async fn save(
        &self,
        id_hash: &str,
        record: &SessionRecord,
        ttl: Duration,
    ) -> Result<(), StoreError>;

    /// Replace the record under `id_hash` only if one is stored, for
    /// `ttl`; `Ok(false)` when there is none.
    ///
    /// The layer writes existing sessions through this method, so a slow
    /// request that loaded a session before another request logged it
    /// out or rotated its id cannot store it again. Implementations must
    /// never let a deleted id become loadable again, even when the check
    /// and the write race with [`delete`](Self::delete).
    ///
    /// # Errors
    /// [`StoreError`] when the record may not have been stored.
    async fn update(
        &self,
        id_hash: &str,
        record: &SessionRecord,
        ttl: Duration,
    ) -> Result<bool, StoreError>;

    /// Remove `id_hash`. Removing an absent record is not an error.
    ///
    /// # Errors
    /// [`StoreError`] when the record may still exist.
    async fn delete(&self, id_hash: &str) -> Result<(), StoreError>;

    /// Invalidate every session of `user`, now and for stale in-flight
    /// writes. Use it after password changes and account recovery.
    ///
    /// # Errors
    /// [`StoreError`] when the revocation may not have taken effect.
    async fn revoke_user(&self, user: &str) -> Result<(), StoreError>;

    /// The user's current revocation epoch, recorded in
    /// [`SessionRecord::auth_epoch`] at login. Stores that revoke by
    /// deleting records can keep the default of `0`.
    ///
    /// # Errors
    /// [`StoreError`] when the epoch cannot be read; the login then fails.
    async fn auth_epoch(&self, _user: &str) -> Result<u64, StoreError> {
        Ok(0)
    }
}
