//! Native SQLite adapter (feature `sqlite-native`), calling SQLite in process
//! through rusqlite instead of SQLx's per-connection worker thread.
//!
//! SQLite runs inside the process, so the cost of a point read is mostly the
//! driver's plumbing. SQLx hands every statement to a worker thread over a
//! channel and waits for the answer; this adapter runs it on the calling
//! task. A primary-key lookup drops from about a hundred microseconds to a
//! few.
//!
//! # Connections
//!
//! - **Readers**: [`NativeSqliteOptions::readers`] connections serve
//!   [`fetch`](Executor::fetch) and [`fetch_raw`](Executor::fetch_raw).
//!   Checking one out is a semaphore permit and a pop from a free list;
//!   when all are busy the caller waits asynchronously, up to
//!   [`acquire_timeout`](NativeSqliteOptions::acquire_timeout).
//! - **Writer**: one connection serves every write, transaction and
//!   transactional schema change. Writers queue for it in FIFO order behind
//!   the same write gate as [`SqliteBackend`](super::SqliteBackend) instead
//!   of racing for SQLite's file lock, and give up after the write timeout
//!   (five seconds, [`with_write_timeout`](NativeSqliteBackend::with_write_timeout)).
//!   A transaction holds the writer and its place in the queue until it
//!   ends; dropped without commit, it is rolled back before either is
//!   released. A non-transactional schema connection opens a connection of
//!   its own and does not queue, as with the SQLx backend.
//! - **In-memory databases** (`sqlite::memory:`, `mode=memory`) exist per
//!   connection, so one connection serves reads and writes alike, as with
//!   the SQLx backend's pool of one. A read waits while a transaction holds
//!   it.
//!
//! Readers see the last committed state. With a rollback journal (SQLite's
//! default) a writer that commits needs every reader out of the file, so
//! reads and commits still contend through SQLite's busy handler; use
//! [`JournalMode::Wal`] when reads and writes overlap.
//!
//! # Where statements run
//!
//! [`Dispatch`] picks, separately for reads and writes, whether a statement
//! runs on the calling task ([`Dispatch::Inline`]) or on Tokio's blocking
//! pool ([`Dispatch::Blocking`]). Inline is the fast path: no thread hop, no
//! allocation for the hand-off. Its cost is that the runtime worker thread
//! is busy for as long as SQLite is: a slow query (a large scan, an
//! unindexed join, a commit waiting on `fsync`) stalls every other task on
//! that worker for its duration, and on a current-thread runtime stalls
//! everything. Reads default to inline, which suits indexed lookups and
//! short page reads; writes default to the blocking pool because a commit
//! can sync to disk. Switch reads to [`Dispatch::Blocking`] for analytical
//! queries.
//!
//! An inline statement cannot be cancelled once it has started: dropping
//! the future does nothing until SQLite returns. A cancelled blocking
//! statement still runs to completion on its thread; inside a transaction
//! the transaction is then rolled back and later calls on it fail with
//! [`QueryError::TransactionAborted`](siderite_orm::QueryError::TransactionAborted).
//!
//! # Single-process mode
//!
//! With WAL, every statement on a reader opens and closes a read
//! transaction, and by default each of those locks a slot in the shared
//! WAL index (the `-shm` file) with `fcntl` while holding a mutex every
//! connection in the process shares. Under load that system call, and the
//! threads queueing behind it, is most of what the read transaction costs.
//!
//! [`NativeSqliteOptions::single_process`] opens the database through
//! SQLite's `unix-excl` VFS instead. On first use the process takes an
//! exclusive POSIX lock on the database file and keeps it until its last
//! connection to the file closes; from then on the locks between
//! connections are bookkeeping in memory, and the WAL index lives in heap
//! memory (no `-shm` file is created). Inside the process nothing changes:
//! the readers, the writer and schema connections still run side by side,
//! readers still see every commit as soon as it is made, reads still run
//! while a write transaction is open, and checkpoints run as usual. Other
//! SQLite connections the same process opens to the file share the
//! in-memory locks and keep working.
//!
//! What it costs: **no other process can use the database while this one
//! has it open** — no second server process, no CLI migration run, no
//! `sqlite3` shell, no backup or replication tool that reads the WAL. A
//! process that tries is held off by the lock: its read waits out its own
//! busy timeout and fails with "database is locked". The other way round,
//! if another process is using the database when this backend connects,
//! [`connect_with`](NativeSqliteBackend::connect_with) waits out
//! [`busy_timeout`](NativeSqliteOptions::busy_timeout) and fails the same
//! way. The lock belongs to the process, so the kernel drops it when the
//! process exits, cleanly or not.
//!
//! Durability is unchanged: the journal, the syncs and what a commit
//! writes are the same. Only the WAL index, which SQLite can rebuild from
//! the WAL, moves to memory; after a crash the next process to open the
//! database rebuilds it, as SQLite always does after a crash, and every
//! committed transaction is there.
//!
//! It applies to file databases on Unix opened read-write (a `mode=ro`
//! handle takes no process lock, so that combination is rejected), cannot
//! be combined with a different `vfs=` in the URL, and is ignored for
//! in-memory databases.
//!
//! # Parity with the SQLx backend
//!
//! Values bind and decode identically (by storage class), errors classify
//! the same way, multi-statement SQL runs every statement with parameters
//! numbered across them, and schema transactions turn foreign keys off and
//! check for new violations before commit. Each connection keeps a cache of
//! prepared statements
//! ([`statement_cache`](NativeSqliteOptions::statement_cache)), so repeated
//! SQL skips parsing and planning.
//!
//! Group commit ([`GroupCommit`](super::GroupCommit)) is not available on
//! this backend yet. Writes already funnel through the single writer
//! connection behind the write gate, which is where a committer task would
//! sit, so it can be added without changing this API.

mod error;
mod exec;
mod open;
mod pool;
#[cfg(test)]
mod tests;
mod tx;

use super::gate::{self, WriteGate};
use crate::sql::{Sqlite, compile, compile_write};
use async_trait::async_trait;
use exec::{dispatch, execute_rows, fetch_rows, script, write_fn};
use open::Target;
use pool::{Lease, Pool};
use siderite_orm::{Backend, BackendCapabilities, BackendError, ExecResult, Executor};
use siderite_orm::{Feature, WritePlan};
use siderite_orm::{IsolationLevel, OrmError, QueryPlan, QueryResult, Transaction, Value};
use std::fmt::{self, Debug, Formatter};
use std::sync::Arc;
use std::time::Duration;
use tx::NativeTransaction;

/// Where a statement runs; see the [module docs](self#where-statements-run).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    /// On the calling task. Fastest; blocks the runtime worker thread for
    /// the duration of the statement.
    Inline,
    /// On Tokio's blocking thread pool (`spawn_blocking`). Costs a thread
    /// hand-off (a few microseconds); keeps the runtime responsive during
    /// slow statements and disk syncs.
    Blocking,
}

/// SQLite's `journal_mode` pragma. See the
/// [SQLite documentation](https://www.sqlite.org/pragma.html#pragma_journal_mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalMode {
    /// Rollback journal deleted after each transaction (SQLite's default).
    Delete,
    /// Rollback journal truncated instead of deleted.
    Truncate,
    /// Rollback journal header zeroed instead of deleted.
    Persist,
    /// Rollback journal kept in memory; a crash can corrupt the database.
    Memory,
    /// Write-ahead log: readers do not block the writer and the writer does
    /// not block readers. Recorded in the database file; does not work on
    /// network filesystems.
    Wal,
    /// No journal; a crash or `ROLLBACK` can corrupt the database.
    Off,
}

impl JournalMode {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Delete => "DELETE",
            Self::Truncate => "TRUNCATE",
            Self::Persist => "PERSIST",
            Self::Memory => "MEMORY",
            Self::Wal => "WAL",
            Self::Off => "OFF",
        }
    }
}

/// SQLite's `synchronous` pragma. See the
/// [SQLite documentation](https://www.sqlite.org/pragma.html#pragma_synchronous).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Synchronous {
    /// No syncs; an OS crash or power loss can corrupt the database.
    Off,
    /// Sync at critical moments only. With WAL, durable against application
    /// crashes but the most recent commits can be lost on power loss.
    Normal,
    /// Sync on every commit (SQLite's default).
    Full,
    /// Like `Full`, and also syncs the directory after deleting a rollback
    /// journal.
    Extra,
}

impl Synchronous {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Off => "OFF",
            Self::Normal => "NORMAL",
            Self::Full => "FULL",
            Self::Extra => "EXTRA",
        }
    }
}

/// Connection setup and dispatch for [`NativeSqliteBackend`].
///
/// The defaults match [`SqliteBackend::connect`](super::SqliteBackend::connect):
/// foreign keys on, a five second busy timeout, and SQLite's own journal
/// mode and `synchronous` setting.
#[derive(Debug, Clone)]
pub struct NativeSqliteOptions {
    /// Reader connections for a file database; at least one. In-memory
    /// databases always use a single connection.
    pub readers: usize,
    /// `PRAGMA journal_mode` for every connection; `None` keeps the
    /// database's current mode. WAL is recorded in the file.
    pub journal_mode: Option<JournalMode>,
    /// `PRAGMA synchronous` for every connection; `None` keeps SQLite's
    /// default (`FULL`). `Normal` with WAL can lose the most recent commits
    /// on power loss.
    pub synchronous: Option<Synchronous>,
    /// `PRAGMA foreign_keys` for every connection.
    pub foreign_keys: bool,
    /// How long SQLite retries a locked database file before failing with
    /// "database is locked".
    pub busy_timeout: Duration,
    /// Prepared statements cached per connection.
    pub statement_cache: usize,
    /// How long a read waits for a free connection (and, for an in-memory
    /// database, a writer for the one connection) before failing.
    pub acquire_timeout: Duration,
    /// Where reads run.
    pub reads: Dispatch,
    /// Where writes, transactions and schema changes run.
    pub writes: Dispatch,
    /// Single-process mode (SQLite's `unix-excl` VFS): cheaper read
    /// transactions, at the price of locking every other process out of
    /// the database while this one has it open. Off by default.
    ///
    /// Another process (a second server, a CLI migration, a `sqlite3`
    /// shell) that tries to read waits out its busy timeout and fails with
    /// "database is locked"; if one is already using the database,
    /// connecting fails that way after
    /// [`busy_timeout`](Self::busy_timeout). Durability is unchanged. Unix
    /// only, read-write file databases only. See
    /// [single-process mode](self#single-process-mode).
    pub single_process: bool,
}

impl Default for NativeSqliteOptions {
    fn default() -> Self {
        Self {
            readers: 10,
            journal_mode: None,
            synchronous: None,
            foreign_keys: true,
            busy_timeout: Duration::from_secs(5),
            statement_cache: 100,
            acquire_timeout: Duration::from_secs(30),
            reads: Dispatch::Inline,
            writes: Dispatch::Blocking,
            single_process: false,
        }
    }
}

/// SQLite adapter calling SQLite in process; see the [module docs](self).
///
/// Clones share the connections and the write queue.
#[derive(Clone)]
pub struct NativeSqliteBackend {
    shared: Arc<Shared>,
    gate: WriteGate,
}

/// State shared by a backend, its clones and its transactions.
struct Shared {
    readers: Arc<Pool>,
    writer: Arc<Pool>,
    target: Arc<Target>,
    acquire_timeout: Duration,
    reads: Dispatch,
    writes: Dispatch,
}

impl NativeSqliteBackend {
    /// Connect to `url` with [`NativeSqliteOptions::default`].
    ///
    /// Accepts the URLs [`SqliteBackend::connect`](super::SqliteBackend::connect)
    /// accepts: `sqlite::memory:`, `sqlite://app.db`, `sqlite:app.db`, with
    /// `mode` (`ro`, `rw`, `rwc`, `memory`), `cache`, `immutable` and `vfs`
    /// query parameters.
    ///
    /// # Errors
    /// Returns [`BackendError::Connection`] if the URL is invalid or the
    /// database cannot be opened.
    pub async fn connect(url: &str) -> Result<Self, BackendError> {
        Self::connect_with(url, NativeSqliteOptions::default()).await
    }

    /// Connect to `url` with explicit options, for example write-ahead
    /// logging:
    ///
    /// ```no_run
    /// # use siderite_backends::sqlite::native::{JournalMode, NativeSqliteBackend, NativeSqliteOptions};
    /// # async fn open() -> Result<(), siderite_orm::BackendError> {
    /// let db = NativeSqliteBackend::connect_with(
    ///     "sqlite://app.db?mode=rwc",
    ///     NativeSqliteOptions {
    ///         journal_mode: Some(JournalMode::Wal),
    ///         ..NativeSqliteOptions::default()
    ///     },
    /// )
    /// .await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Every connection opens now, on the blocking pool, so a bad path or
    /// pragma fails here. A connection that later has to be discarded (its
    /// transaction could not be rolled back) is reopened on checkout, on the
    /// calling task.
    ///
    /// # Errors
    /// Returns [`BackendError::Connection`] if the URL or the options are
    /// invalid, or the database cannot be opened and configured.
    pub async fn connect_with(
        url: &str,
        options: NativeSqliteOptions,
    ) -> Result<Self, BackendError> {
        if options.readers == 0 {
            return Err(BackendError::Connection(
                "native SQLite needs at least one reader connection".into(),
            ));
        }
        let target = Arc::new(Target::parse(url, &options)?);
        // One connection holds an in-memory database; it cannot be reopened.
        let count = if target.in_memory() {
            1
        } else {
            1 + options.readers
        };
        let opener = Arc::clone(&target);
        let mut conns = tokio::task::spawn_blocking(move || {
            (0..count)
                .map(|_| opener.open())
                .collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(|e| BackendError::Connection(e.to_string()))?
        .map_err(|e| BackendError::Connection(error::map(e).to_string()))?;
        let (readers, writer) = if target.in_memory() {
            let only = Arc::new(Pool::new(conns, 1, None));
            (Arc::clone(&only), only)
        } else {
            let writer = conns.split_off(options.readers);
            let readers = Pool::new(conns, options.readers, Some(Arc::clone(&target)));
            let writer = Pool::new(writer, 1, Some(Arc::clone(&target)));
            (Arc::new(readers), Arc::new(writer))
        };
        Ok(Self {
            shared: Arc::new(Shared {
                readers,
                writer,
                target,
                acquire_timeout: options.acquire_timeout,
                reads: options.reads,
                writes: options.writes,
            }),
            gate: WriteGate::new(gate::DEFAULT_WAIT),
        })
    }

    /// How long a write waits for the writer connection before failing with
    /// "database is locked"; five seconds by default, like the busy timeout.
    ///
    /// The queue is shared with existing clones; only this handle's wait
    /// changes.
    #[must_use]
    pub fn with_write_timeout(mut self, wait: Duration) -> Self {
        self.gate = self.gate.with_wait(wait);
        self
    }

    /// A reader connection, waiting if every one is busy.
    async fn reader(&self) -> Result<Lease, OrmError> {
        let shared = &self.shared;
        match shared.readers.try_lease(shared.reads) {
            Some(lease) => lease,
            None => {
                let wait = shared.readers.lease(shared.reads, shared.acquire_timeout);
                wait.await
            }
        }
    }

    /// The writer connection, after queueing for the write gate.
    async fn writer(&self) -> Result<Lease, OrmError> {
        let permit = self.gate.acquire().await?;
        let shared = &self.shared;
        let lease = match shared.writer.try_lease(shared.writes) {
            Some(lease) => lease?,
            None => {
                let wait = shared.writer.lease(shared.writes, shared.acquire_timeout);
                wait.await?
            }
        };
        Ok(lease.with_gate(permit))
    }
}

impl Debug for NativeSqliteBackend {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeSqliteBackend")
            .field("target", &self.shared.target)
            .field("reads", &self.shared.reads)
            .field("writes", &self.shared.writes)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Executor for NativeSqliteBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::sqlite()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        let compiled = compile(plan, &Sqlite)?;
        let lease = self.reader().await?;
        dispatch(
            self.shared.reads,
            lease,
            &compiled.sql,
            compiled.params,
            fetch_rows,
        )
        .await
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        let compiled = compile_write(plan, &Sqlite)?;
        let run = write_fn(!plan.returning().is_empty());
        let lease = self.writer().await?;
        dispatch(
            self.shared.writes,
            lease,
            &compiled.sql,
            compiled.params,
            run,
        )
        .await
    }

    /// Runs on a reader connection without queueing for the writer, as the
    /// SQLx backend's `fetch_raw` bypasses its write queue: a write sent
    /// here competes for SQLite's file lock through the busy handler.
    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        let lease = self.reader().await?;
        dispatch(self.shared.reads, lease, sql, params, fetch_rows).await
    }

    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        let lease = self.writer().await?;
        dispatch(self.shared.writes, lease, sql, params, execute_rows).await
    }

    async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        let lease = self.writer().await?;
        dispatch(self.shared.writes, lease, sql, Vec::new(), script).await
    }
}

#[async_trait]
impl Backend for NativeSqliteBackend {
    fn read_parameter_count(&self, plan: &QueryPlan) -> Result<Option<usize>, OrmError> {
        Ok(Some(compile(plan, &Sqlite)?.params.len()))
    }

    async fn begin(
        &self,
        isolation: Option<IsolationLevel>,
    ) -> Result<Box<dyn Transaction>, OrmError> {
        if let Some(level) = isolation {
            self.capabilities().require(Feature::Isolation(level))?;
        }
        // Held for the whole transaction: it may write at any point.
        let lease = self.writer().await?;
        let tx = NativeTransaction::begin(lease, self.shared.reads, self.shared.writes).await?;
        Ok(Box::new(tx))
    }

    async fn begin_schema(&self, transactional: bool) -> Result<Box<dyn Transaction>, OrmError> {
        let shared = &self.shared;
        let lease = if transactional {
            self.writer().await?
        } else if shared.target.in_memory() {
            // The one connection; other writes wait for it, as with a SQLx
            // pool of one.
            shared
                .writer
                .lease(shared.writes, shared.acquire_timeout)
                .await?
        } else {
            // Its own connection, closed afterwards: like the SQLx backend,
            // a non-transactional schema connection leaves writes open.
            Lease::dedicated(Arc::clone(&shared.target), shared.writes).await?
        };
        let tx = NativeTransaction::begin_schema(lease, transactional, shared.reads, shared.writes);
        Ok(Box::new(tx.await?))
    }
}
