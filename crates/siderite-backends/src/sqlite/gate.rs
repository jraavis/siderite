//! In-process serialization of SQLite writers.
//!
//! SQLite admits one writer per database file. Pooled connections that write
//! at the same time race for the file lock, and the losers sleep in SQLite's
//! busy handler (1, 2, 5 … 100 ms) before retrying. Under load that sleeping
//! costs throughput and adds a long latency tail. Queuing writers here in
//! FIFO order instead hands the lock over without sleeping.
//!
//! The gate covers writers of one [`SqliteBackend`](super::SqliteBackend) and
//! its clones only. Other processes, other backends on the same file and, in
//! rollback-journal mode, readers holding a shared lock still contend through
//! SQLite's busy handler.

use siderite_orm::{BackendError, OrmError};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// SQLx's default SQLite busy timeout, so a queued writer gives up no sooner
/// than one waiting on the file lock would.
pub(super) const DEFAULT_WAIT: Duration = Duration::from_secs(5);

/// One-writer FIFO queue shared by a backend and its clones.
#[derive(Debug, Clone)]
pub(super) struct WriteGate {
    permits: Arc<Semaphore>,
    wait: Duration,
}

/// Write access held until dropped.
pub(super) type WritePermit = OwnedSemaphorePermit;

impl WriteGate {
    pub(super) fn new(wait: Duration) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(1)),
            wait,
        }
    }

    pub(super) fn with_wait(&self, wait: Duration) -> Self {
        Self {
            permits: Arc::clone(&self.permits),
            wait,
        }
    }

    /// Wait for the single write slot.
    ///
    /// # Errors
    /// After the wait deadline, the same "database is locked" failure a busy
    /// connection reports. This also covers a task that writes outside a
    /// transaction it holds open, which would otherwise wait forever.
    pub(super) async fn acquire(&self) -> Result<WritePermit, OrmError> {
        let permits = Arc::clone(&self.permits);
        match tokio::time::timeout(self.wait, permits.acquire_owned()).await {
            Ok(Ok(permit)) => Ok(permit),
            // The semaphore is never closed.
            Ok(Err(_)) => Err(locked(self.wait)),
            Err(_) => Err(locked(self.wait)),
        }
    }
}

fn locked(wait: Duration) -> OrmError {
    BackendError::Database(format!(
        "database is locked: no SQLite write slot within {} ms",
        wait.as_millis()
    ))
    .into()
}
