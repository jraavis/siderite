//! Transactions and schema connections on a held connection.
//!
//! A transaction owns its [`Lease`] (the writer connection and the write
//! permit) until it commits or rolls back. Dropped without either, the lease
//! rolls back and, for a schema transaction, restores `PRAGMA foreign_keys`
//! before the connection and the permit are released.

use super::Dispatch;
use super::exec::{self, Run, blocking, execute_rows, fetch_rows, script, write_fn};
use super::pool::Lease;
use crate::sql::{Sqlite, compile, compile_write};
use crate::sqlite::schema::{self, FOREIGN_KEY_CHECK, FOREIGN_KEYS, FkViolation};
use async_trait::async_trait;
use rusqlite::Connection;
use siderite_orm::{BackendCapabilities, BackendError, ExecResult, Executor, OrmError};
use siderite_orm::{QueryError, QueryPlan, QueryResult, RowSink, Transaction, Value, WritePlan};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Where the held connection is.
enum Slot {
    Open(Lease),
    /// A cancelled statement took the connection with it; the transaction
    /// was rolled back when that statement finished.
    Lost,
    Closed,
}

impl Slot {
    /// Take the lease out, leaving `rest` behind.
    fn take(&mut self, rest: Slot) -> Result<Lease, QueryError> {
        match std::mem::replace(self, rest) {
            Slot::Open(lease) => Ok(lease),
            gone => {
                let error = gone.error();
                *self = gone;
                Err(error)
            }
        }
    }

    fn error(&self) -> QueryError {
        match self {
            Slot::Lost => QueryError::TransactionAborted,
            Slot::Open(_) | Slot::Closed => QueryError::TransactionClosed,
        }
    }
}

/// Foreign-key state of a schema connection; see [`schema`].
#[derive(Debug)]
struct SchemaState {
    restore_fk: i64,
    in_txn: bool,
    baseline: Option<HashSet<FkViolation>>,
}

/// An open transaction or schema connection.
pub(super) struct NativeTransaction {
    slot: Mutex<Slot>,
    reads: Dispatch,
    writes: Dispatch,
    schema: Option<Arc<SchemaState>>,
}

impl NativeTransaction {
    /// `BEGIN` on `lease`.
    pub(super) async fn begin(
        lease: Lease,
        reads: Dispatch,
        writes: Dispatch,
    ) -> Result<Self, OrmError> {
        let (lease, ()) = hold(writes, lease, |lease| {
            script(lease.conn()?, "BEGIN", Vec::new())
        })
        .await?;
        Ok(Self {
            slot: Mutex::new(Slot::Open(lease)),
            reads,
            writes,
            schema: None,
        })
    }

    /// Turn foreign keys off on `lease`, record the violations already
    /// there, and `BEGIN IMMEDIATE` when `transactional`.
    pub(super) async fn begin_schema(
        lease: Lease,
        transactional: bool,
        reads: Dispatch,
        writes: Dispatch,
    ) -> Result<Self, OrmError> {
        let (lease, state) = hold(writes, lease, move |lease| {
            open_schema(lease, transactional)
        })
        .await?;
        Ok(Self {
            slot: Mutex::new(Slot::Open(lease)),
            reads,
            writes,
            schema: Some(Arc::new(state)),
        })
    }

    /// Run `run` on the held connection where `mode` says.
    async fn run<T: Send + 'static>(
        &self,
        mode: Dispatch,
        sql: &str,
        params: Vec<Value>,
        run: Run<T>,
    ) -> Result<T, OrmError> {
        let mut slot = self.slot.lock().await;
        match mode {
            Dispatch::Inline => match &*slot {
                Slot::Open(lease) => run(lease.conn()?, sql, params),
                gone => Err(gone.error().into()),
            },
            Dispatch::Blocking => {
                // Cancelled while out, the slot stays `Lost`.
                let lease = slot.take(Slot::Lost)?;
                let sql = sql.to_owned();
                let (lease, done) = blocking(move || {
                    let done = lease
                        .conn()
                        .map_err(OrmError::from)
                        .and_then(|conn| run(conn, &sql, params));
                    Ok((lease, done))
                })
                .await?;
                *slot = Slot::Open(lease);
                done
            }
        }
    }

    /// [`exec::fetch_into`] on the held connection; off the calling task
    /// the rows are fetched there and handed to `sink` afterwards.
    async fn run_into(
        &self,
        sql: &str,
        params: Vec<Value>,
        sink: &mut dyn RowSink,
    ) -> Result<(), OrmError> {
        if self.reads == Dispatch::Inline {
            return match &*self.slot.lock().await {
                Slot::Open(lease) => exec::fetch_into(lease.conn()?, sql, params, sink),
                gone => Err(gone.error().into()),
            };
        }
        for mut row in self.run(self.reads, sql, params, fetch_rows).await?.rows {
            sink.push(&mut row)?;
        }
        Ok(())
    }

    async fn finish(&self, commit: bool) -> Result<(), OrmError> {
        let lease = self.slot.lock().await.take(Slot::Closed)?;
        let schema = self.schema.clone();
        let (_lease, ()) = hold(self.writes, lease, move |lease| match &schema {
            Some(state) if commit => commit_schema(lease, state),
            Some(state) => rollback_schema(lease, state),
            None => script(
                lease.conn()?,
                if commit { "COMMIT" } else { "ROLLBACK" },
                Vec::new(),
            ),
        })
        .await?;
        // `_lease` drops here: back to its pool, permit released.
        Ok(())
    }
}

/// Run `work` on `lease` where `mode` says and give the lease back with its
/// result. On failure the lease is dropped, which resets it.
async fn hold<T, F>(mode: Dispatch, lease: Lease, work: F) -> Result<(Lease, T), OrmError>
where
    T: Send + 'static,
    F: FnOnce(&mut Lease) -> Result<T, OrmError> + Send + 'static,
{
    let mut lease = lease;
    match mode {
        Dispatch::Inline => work(&mut lease).map(|done| (lease, done)),
        Dispatch::Blocking => blocking(move || work(&mut lease).map(|done| (lease, done))).await,
    }
}

fn open_schema(lease: &mut Lease, transactional: bool) -> Result<SchemaState, OrmError> {
    let conn = lease.conn()?;
    let restore_fk = foreign_keys(conn)?;
    // Baseline for the pre-commit check: only *new* violations fail the
    // migration. Skipped when FKs were already off (nothing to enforce).
    let baseline = if restore_fk == 0 {
        None
    } else {
        Some(violations(conn)?.into_iter().collect())
    };
    script(conn, schema::set_foreign_keys_sql(0), Vec::new())?;
    lease.restore_foreign_keys(restore_fk);
    if transactional {
        script(lease.conn()?, "BEGIN IMMEDIATE", Vec::new())?;
    }
    Ok(SchemaState {
        restore_fk,
        in_txn: transactional,
        baseline,
    })
}

/// Check for new violations, commit, restore the pragma. Any failure leaves
/// the rollback and the pragma to the lease's reset.
fn commit_schema(lease: &mut Lease, state: &SchemaState) -> Result<(), OrmError> {
    let conn = lease.conn()?;
    if let Some(baseline) = &state.baseline {
        let fresh = schema::new_violations(baseline, violations(conn)?);
        if !fresh.is_empty() {
            return Err(schema::violation_error(&fresh, state.in_txn));
        }
    }
    if state.in_txn {
        script(conn, "COMMIT", Vec::new())?;
    }
    script(
        conn,
        schema::set_foreign_keys_sql(state.restore_fk),
        Vec::new(),
    )?;
    lease.foreign_keys_restored();
    Ok(())
}

fn rollback_schema(lease: &mut Lease, state: &SchemaState) -> Result<(), OrmError> {
    let conn = lease.conn()?;
    let rollback = if state.in_txn {
        script(conn, "ROLLBACK", Vec::new())
    } else {
        Ok(())
    };
    let restore = script(
        conn,
        schema::set_foreign_keys_sql(state.restore_fk),
        Vec::new(),
    );
    if restore.is_ok() {
        lease.foreign_keys_restored();
    }
    rollback?;
    restore
}

/// `PRAGMA foreign_keys` as `0` or `1`.
fn foreign_keys(conn: &Connection) -> Result<i64, OrmError> {
    let rows = fetch_rows(conn, FOREIGN_KEYS, Vec::new())?.rows;
    match rows.first().and_then(|row| row.column_at(0)) {
        Some((_, Value::Int(on))) => Ok(*on),
        _ => Err(BackendError::Database(
            "no rows returned by a query that expected to return at least one row".into(),
        )
        .into()),
    }
}

fn violations(conn: &Connection) -> Result<Vec<FkViolation>, OrmError> {
    let rows = fetch_rows(conn, FOREIGN_KEY_CHECK, Vec::new())?.rows;
    let text = |value: Option<&Value>| match value {
        Some(Value::Text(text)) => Some(text.clone()),
        _ => None,
    };
    Ok(rows
        .iter()
        .map(|row| {
            let rowid = match row.get("rowid") {
                Some(Value::Int(rowid)) => Some(*rowid),
                _ => None,
            };
            FkViolation::new(text(row.get("table")), rowid, text(row.get("parent")))
        })
        .collect())
}

#[async_trait]
impl Executor for NativeTransaction {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::sqlite()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        let compiled = compile(plan, &Sqlite)?;
        self.run(self.reads, &compiled.sql, compiled.params, fetch_rows)
            .await
    }

    async fn fetch_each(&self, plan: &QueryPlan, sink: &mut dyn RowSink) -> Result<(), OrmError> {
        let compiled = compile(plan, &Sqlite)?;
        self.run_into(&compiled.sql, compiled.params, sink).await
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        let compiled = compile_write(plan, &Sqlite)?;
        let run = write_fn(!plan.returning().is_empty());
        self.run(self.writes, &compiled.sql, compiled.params, run)
            .await
    }

    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        self.run(self.reads, sql, params, fetch_rows).await
    }

    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        self.run(self.writes, sql, params, execute_rows).await
    }

    async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        self.run(self.writes, sql, Vec::new(), exec::script).await
    }
}

#[async_trait]
impl Transaction for NativeTransaction {
    async fn commit(&self) -> Result<(), OrmError> {
        self.finish(true).await
    }

    async fn rollback(&self) -> Result<(), OrmError> {
        self.finish(false).await
    }
}
