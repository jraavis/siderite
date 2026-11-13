//! Connection checkout: a free list behind a semaphore, and the lease that
//! hands a connection back clean.
//!
//! The uncontended checkout is a semaphore `try_acquire` and a `Vec::pop`
//! under a mutex held for nothing else, so it costs tens of nanoseconds.
//! When every connection is out, callers wait on the semaphore (FIFO,
//! asynchronously); the mutex is never held across a statement or an
//! `.await`.

use super::Dispatch;
use super::error;
use super::exec;
use super::open::Target;
use crate::sqlite::gate::WritePermit;
use crate::sqlite::schema;
use rusqlite::Connection;
use siderite_orm::{BackendError, OrmError, QueryError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::Semaphore;

/// A fixed number of interchangeable connections.
pub(super) struct Pool {
    idle: Mutex<Vec<Connection>>,
    permits: Semaphore,
    /// Reopens a connection that had to be discarded. `None` for an
    /// in-memory database, whose one connection holds the data.
    target: Option<Arc<Target>>,
}

impl Pool {
    /// A pool of `size` connections, `idle` of them already open; the rest
    /// open on checkout through `target`.
    pub(super) fn new(idle: Vec<Connection>, size: usize, target: Option<Arc<Target>>) -> Self {
        Self {
            idle: Mutex::new(idle),
            permits: Semaphore::new(size),
            target,
        }
    }

    /// A connection if one is free right now, without waiting.
    ///
    /// Returns `None` when every connection is out, or an error when a
    /// connection had to be opened and could not be.
    pub(super) fn try_lease(
        self: &Arc<Self>,
        cleanup: Dispatch,
    ) -> Option<Result<Lease, OrmError>> {
        let permit = self.permits.try_acquire().ok()?;
        permit.forget();
        Some(self.checkout(cleanup))
    }

    /// A connection, waiting up to `timeout` for one to come back.
    pub(super) async fn lease(
        self: &Arc<Self>,
        cleanup: Dispatch,
        timeout: Duration,
    ) -> Result<Lease, OrmError> {
        match tokio::time::timeout(timeout, self.permits.acquire()).await {
            Ok(Ok(permit)) => permit.forget(),
            // The semaphore is never closed.
            Ok(Err(_)) | Err(_) => return Err(timed_out()),
        }
        self.checkout(cleanup)
    }

    /// Take an idle connection, or open one, for a permit already held.
    fn checkout(self: &Arc<Self>, cleanup: Dispatch) -> Result<Lease, OrmError> {
        let idle = self
            .idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        let conn = match idle {
            Some(conn) => conn,
            None => match self.open() {
                Ok(conn) => conn,
                Err(err) => {
                    self.permits.add_permits(1);
                    return Err(err);
                }
            },
        };
        Ok(Lease {
            conn: Some(conn),
            home: Some(Arc::clone(self)),
            gate: None,
            restore_fk: None,
            cleanup,
        })
    }

    fn open(&self) -> Result<Connection, OrmError> {
        let target = self.target.as_ref().ok_or_else(|| {
            BackendError::Connection("the in-memory SQLite connection was lost".into())
        })?;
        Ok(target.open().map_err(error::map)?)
    }

    /// Return a connection. One that could not be reset is closed instead
    /// and reopened on a later checkout, unless it holds an in-memory
    /// database.
    fn give_back(&self, conn: Connection, clean: bool) {
        if clean || self.target.is_none() {
            self.idle
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(conn);
        } else {
            drop(conn);
        }
        self.permits.add_permits(1);
    }
}

fn timed_out() -> OrmError {
    BackendError::Database("pool timed out while waiting for an open connection".into()).into()
}

/// A checked-out connection.
///
/// Dropping it hands the connection back in a clean state: an open
/// transaction is rolled back and a `PRAGMA foreign_keys` changed by a
/// schema transaction is restored, then the write permit (if any) is
/// released. When that reset needs a statement and `cleanup` is
/// [`Dispatch::Blocking`], it runs on the blocking pool.
pub(super) struct Lease {
    conn: Option<Connection>,
    /// `None` for a dedicated connection, closed when dropped.
    home: Option<Arc<Pool>>,
    gate: Option<WritePermit>,
    restore_fk: Option<i64>,
    cleanup: Dispatch,
}

impl Lease {
    /// A connection of its own, outside the pools, for a non-transactional
    /// schema change on a file database.
    pub(super) async fn dedicated(
        target: Arc<Target>,
        cleanup: Dispatch,
    ) -> Result<Self, OrmError> {
        let conn = match cleanup {
            Dispatch::Inline => target.open(),
            Dispatch::Blocking => {
                let open = move || target.open().map_err(|e| OrmError::from(error::map(e)));
                return exec::blocking(open)
                    .await
                    .map(|conn| Self::own(conn, cleanup));
            }
        };
        Ok(Self::own(conn.map_err(error::map)?, cleanup))
    }

    fn own(conn: Connection, cleanup: Dispatch) -> Self {
        Self {
            conn: Some(conn),
            home: None,
            gate: None,
            restore_fk: None,
            cleanup,
        }
    }

    /// Hold `permit` until this lease is handed back.
    pub(super) fn with_gate(mut self, permit: WritePermit) -> Self {
        self.gate = Some(permit);
        self
    }

    /// The connection.
    ///
    /// # Errors
    /// [`QueryError::TransactionAborted`] once the lease has been handed
    /// back, which only a dropped lease can be.
    pub(super) fn conn(&self) -> Result<&Connection, QueryError> {
        self.conn.as_ref().ok_or(QueryError::TransactionAborted)
    }

    /// Set `PRAGMA foreign_keys` back to `on` when this lease is handed back,
    /// unless [`foreign_keys_restored`](Self::foreign_keys_restored) is called
    /// first.
    pub(super) fn restore_foreign_keys(&mut self, on: i64) {
        self.restore_fk = Some(on);
    }

    /// The foreign-key setting is back to what it was.
    pub(super) fn foreign_keys_restored(&mut self) {
        self.restore_fk = None;
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else {
            return;
        };
        let dirty = !conn.is_autocommit() || self.restore_fk.is_some();
        if dirty
            && self.cleanup == Dispatch::Blocking
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            // If the task never runs (runtime shutting down), dropping the
            // closure drops this lease, which resets inline instead.
            let moved = Self {
                conn: Some(conn),
                home: self.home.take(),
                gate: self.gate.take(),
                restore_fk: self.restore_fk.take(),
                cleanup: Dispatch::Inline,
            };
            drop(runtime.spawn_blocking(move || drop(moved)));
            return;
        }
        let clean = !dirty || reset(&conn, self.restore_fk);
        match &self.home {
            Some(home) => home.give_back(conn, clean),
            None => drop(conn),
        }
        // The write permit is a field: it is released after this returns,
        // once the connection is back.
    }
}

/// Roll back an open transaction and restore `PRAGMA foreign_keys`.
/// Returns whether the connection is clean again.
fn reset(conn: &Connection, restore_fk: Option<i64>) -> bool {
    let rolled_back = conn.is_autocommit() || conn.execute_batch("ROLLBACK").is_ok();
    let restored =
        restore_fk.is_none_or(|on| conn.execute_batch(schema::set_foreign_keys_sql(on)).is_ok());
    rolled_back && restored
}
