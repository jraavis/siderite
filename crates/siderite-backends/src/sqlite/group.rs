//! Opt-in group commit for autocommit SQLite writes.
//!
//! Every autocommit write is its own transaction, so each pays SQLite's
//! commit sync (several `fsync`s in rollback-journal mode). With group commit
//! one task performs the writes: whatever queued while the previous commit
//! was running shares the next transaction, and therefore one commit sync.
//!
//! - Each write runs inside its own savepoint, so a failing statement fails
//!   only its own caller; the rest of the batch still commits.
//! - A caller is answered only after the shared `COMMIT` succeeds, so an
//!   acknowledged write is as durable as an ungrouped one.
//! - If `COMMIT` fails, every caller in the batch gets the error. Nothing is
//!   retried: the outcome of a failed commit is not always known.
//! - A lone queued write runs as a plain autocommit statement, so light load
//!   pays no transaction or savepoint overhead.
//!
//! What changes: writes in one batch become visible together, a commit
//! failure fails every write in the batch, and a statement that ends the
//! surrounding transaction itself (`INSERT OR ROLLBACK`, `RAISE(ROLLBACK)`)
//! fails the writes queued with it.

use super::gate::WriteGate;
use super::run_write;
use crate::shared::map_error;
use crate::sql::CompiledQuery;
use siderite_orm::{BackendError, ExecResult, OrmError};
use sqlx::TransactionManager;
use sqlx::sqlite::{SqliteConnection, SqlitePool, SqliteTransactionManager};
use tokio::sync::{mpsc, oneshot};

/// Group commit settings for
/// [`SqliteBackend::group_commit`](super::SqliteBackend::group_commit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupCommit {
    /// Most writes sharing one transaction.
    pub max_batch: usize,
    /// Writes waiting for the committer before callers wait to enqueue.
    pub queue: usize,
}

impl Default for GroupCommit {
    fn default() -> Self {
        Self {
            max_batch: 64,
            queue: 1024,
        }
    }
}

type Reply = Result<ExecResult, OrmError>;

struct Job {
    compiled: CompiledQuery,
    returning: bool,
    reply: oneshot::Sender<Reply>,
}

/// Handle to the committer task. The task ends once every handle is gone.
#[derive(Debug, Clone)]
pub(super) struct Committer {
    jobs: mpsc::Sender<Job>,
}

impl Committer {
    /// Start the committer on the current Tokio runtime.
    pub(super) fn spawn(pool: SqlitePool, gate: WriteGate, config: GroupCommit) -> Self {
        let max_batch = config.max_batch.max(1);
        let (jobs, queue) = mpsc::channel(config.queue.max(1));
        tokio::spawn(run(pool, gate, queue, max_batch));
        Self { jobs }
    }

    /// Queue one write and wait for its committed result.
    pub(super) async fn submit(&self, compiled: CompiledQuery, returning: bool) -> Reply {
        let (reply, answer) = oneshot::channel();
        let job = Job {
            compiled,
            returning,
            reply,
        };
        self.jobs.send(job).await.map_err(|_| stopped())?;
        answer.await.map_err(|_| stopped())?
    }
}

fn stopped() -> OrmError {
    BackendError::Database("SQLite group committer stopped".into()).into()
}

async fn run(pool: SqlitePool, gate: WriteGate, mut queue: mpsc::Receiver<Job>, max: usize) {
    let mut batch = Vec::with_capacity(max);
    while queue.recv_many(&mut batch, max).await > 0 {
        // A caller that gave up before its write started never gets one,
        // as with an ungrouped write cancelled before it was sent.
        batch.retain(|job| !job.reply.is_closed());
        if batch.is_empty() {
            continue;
        }
        let replies = match gate.acquire().await {
            Ok(_permit) => match pool.acquire().await {
                Ok(mut conn) => write(&mut conn, &mut batch).await,
                Err(err) => fail_all(batch.len(), &OrmError::from(map_error(err))),
            },
            Err(err) => fail_all(batch.len(), &err),
        };
        for (job, reply) in batch.drain(..).zip(replies) {
            drop(job.reply.send(reply));
        }
    }
}

async fn write(conn: &mut SqliteConnection, batch: &mut [Job]) -> Vec<Reply> {
    if let [job] = batch {
        let compiled = take(&mut job.compiled);
        return vec![run_write(&mut *conn, compiled, job.returning).await];
    }
    if let Err(err) = SqliteTransactionManager::begin(conn, Some("BEGIN IMMEDIATE".into())).await {
        return fail_all(batch.len(), &OrmError::from(map_error(err)));
    }
    let mut replies = Vec::with_capacity(batch.len());
    for job in batch.iter_mut() {
        if let Err(err) = savepoint(conn, "SAVEPOINT siderite_group").await {
            return abort(conn, batch.len(), &err).await;
        }
        let compiled = take(&mut job.compiled);
        let reply = run_write(&mut *conn, compiled, job.returning).await;
        let close = if reply.is_ok() {
            savepoint(conn, "RELEASE siderite_group").await
        } else {
            // Fails when the statement ended the whole transaction.
            match savepoint(conn, "ROLLBACK TO siderite_group").await {
                Ok(()) => savepoint(conn, "RELEASE siderite_group").await,
                Err(err) => Err(err),
            }
        };
        if let Err(err) = close {
            return abort(conn, batch.len(), &err).await;
        }
        replies.push(reply);
    }
    match SqliteTransactionManager::commit(conn).await {
        Ok(()) => replies,
        Err(err) => abort(conn, batch.len(), &OrmError::from(map_error(err))).await,
    }
}

/// Roll the batch back and fail every caller with `err`.
async fn abort(conn: &mut SqliteConnection, len: usize, err: &OrmError) -> Vec<Reply> {
    drop(SqliteTransactionManager::rollback(conn).await);
    fail_all(len, err)
}

fn fail_all(len: usize, err: &OrmError) -> Vec<Reply> {
    let message = format!("SQLite group commit failed: {err}");
    (0..len)
        .map(|_| Err(BackendError::Database(message.clone()).into()))
        .collect()
}

async fn savepoint(conn: &mut SqliteConnection, sql: &str) -> Result<(), OrmError> {
    sqlx::Executor::execute(&mut *conn, sql)
        .await
        .map(|_| ())
        .map_err(|e| map_error(e).into())
}

fn take(compiled: &mut CompiledQuery) -> CompiledQuery {
    CompiledQuery {
        sql: std::mem::take(&mut compiled.sql),
        params: std::mem::take(&mut compiled.params),
    }
}
