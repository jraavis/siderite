//! Apply and reverse migrations against a [`siderite_orm::Db`].

use crate::error::MigrationError;
use crate::loader::{MigrationGraph, is_applied};
use crate::migration::Migration;
use crate::operation::Operation;
use crate::registry::MigrationRegistry;
use crate::schema_editor;
use crate::state::ProjectState;
use chrono::Utc;
use siderite_orm::types::TIMESTAMP_FORMAT;
use siderite_orm::{BackendKind, Db, TransactionSupport, Value};
use std::collections::{HashMap, HashSet};
use std::future::Future;

mod sqlite;
use sqlite::{
    load_sqlite_rebuild_extras, reject_lossy_sqlite_cast, reject_not_null_violations,
    restore_sqlite_rebuild_extras,
};
mod operations;
use operations::{apply_ops_in_order, unapply_ops_in_order};
mod progress;
use progress::{
    PROGRESS_APPLY, PROGRESS_UNAPPLY, Progress, clear_progress, load_progress,
    reset_progress_direction, save_progress,
};
mod catalog;
mod intents;
pub use intents::MigrationIntent;
mod recovery;
pub use recovery::{RecoveryEntry, RecoveryReport};
mod holder;
pub use holder::LockHolder;
mod locks;
use locks::release_migrate_lock;

#[cfg(test)]
mod intent_tests;
#[cfg(test)]
mod tests;

/// History table written by the migrator.
pub const HISTORY_TABLE: &str = "siderite_migrations";

/// Outcome of [`Migrator::migrate`] or [`Migrator::rollback`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// Migration ids that would be (or were) applied / unapplied, in order.
    pub planned: Vec<String>,
    /// Generated SQL, including the history-table DDL when it would be created.
    pub sql: Vec<String>,
    /// Ids actually written to (or deleted from) the history table.
    pub applied: Vec<String>,
    /// When true, no statement was executed.
    pub dry_run: bool,
}

/// Applies JSON migrations through [`Db`].
pub struct Migrator<'a> {
    db: &'a Db,
    graph: &'a MigrationGraph,
    registry: MigrationRegistry,
    lock_timeout: std::time::Duration,
}

impl<'a> Migrator<'a> {
    /// Plan and execute using `graph`.
    pub fn new(db: &'a Db, graph: &'a MigrationGraph) -> Self {
        Self {
            db,
            graph,
            registry: MigrationRegistry::new(),
            lock_timeout: std::time::Duration::from_secs(30),
        }
    }

    /// Register `RunRust` functions used by the planned migrations.
    #[must_use]
    pub fn with_registry(mut self, registry: MigrationRegistry) -> Self {
        self.registry = registry;
        self
    }

    /// Set the advisory lock acquisition deadline (default: 30 seconds).
    ///
    /// Args:
    ///     timeout: Positive, representable PostgreSQL/MySQL lock wait.
    ///
    /// Returns:
    ///     This migrator with the new acquisition policy.
    ///
    /// Pool acquisition and SQLite BEGIN use backend deadlines. This limit
    /// does not bound migration execution after the lock is held.
    ///
    /// # Errors
    /// Zero or a duration that overflows the runtime clock.
    pub fn with_lock_timeout(
        mut self,
        timeout: std::time::Duration,
    ) -> Result<Self, MigrationError> {
        if timeout.is_zero() || tokio::time::Instant::now().checked_add(timeout).is_none() {
            return Err(MigrationError::usage("invalid migration lock timeout"));
        }
        self.lock_timeout = timeout;
        Ok(self)
    }

    /// Ids that would be applied to reach `target` (`None` = all remaining).
    ///
    /// # Errors
    /// Checksum mismatch, unknown target, graph/state errors, or backend errors
    /// while reading the history table.
    pub async fn plan_forward(&self, target: Option<&str>) -> Result<Vec<String>, MigrationError> {
        let history = self.load_history().await?;
        self.verify_checksums(&history)?;
        self.forward_plan(&history, target)
    }

    /// Apply unapplied migrations up to `target` (`None` = all).
    ///
    /// `dry_run` returns the SQL without executing it (history is still read).
    ///
    /// # Errors
    /// See [`Migrator::plan_forward`]; also statement execution errors.
    pub async fn migrate(
        &self,
        target: Option<&str>,
        dry_run: bool,
    ) -> Result<Report, MigrationError> {
        self.require_backend()?;
        if dry_run {
            let history = self.load_history().await?;
            self.verify_checksums(&history)?;
            let plan = self.forward_plan(&history, target)?;
            let (sql, _) = self.plan_sql(&history, &plan)?;
            return Ok(Report {
                planned: plan,
                sql,
                applied: Vec::new(),
                dry_run: true,
            });
        }
        if self.db.is_scoped() {
            return Err(MigrationError::state(
                "migration execution requires a pool handle; \
                 caller-owned transactions and connections are unsupported",
            ));
        }
        self.with_lock(|session| {
            let this = self;
            async move {
                let created_history = if !this.history_table_exists_on(&session).await? {
                    session
                        .execute_script(&create_history_sql(this.kind()))
                        .await?;
                    true
                } else {
                    false
                };
                let history = this.load_history_from(&session).await?;
                this.verify_checksums(&history)?;
                let plan = this.forward_plan(&history, target)?;
                // Render under the lock, from the plan re-read here:
                // `Report.sql` must not disagree with `Report.planned`
                // about what actually ran.
                let (mut sql, mut state) = this.plan_sql(&history, &plan)?;
                if created_history {
                    sql.insert(0, create_history_sql(this.kind()));
                }
                let mut applied = Vec::new();
                for id in &plan {
                    let migration = this.require_migration(id)?.clone();
                    this.apply_one(&session, &migration, &mut state).await?;
                    applied.push(id.clone());
                }
                Ok(Report {
                    planned: plan,
                    sql,
                    applied,
                    dry_run: false,
                })
            }
        })
        .await
    }

    /// Unapply migrations. `target` is the id that should remain the head
    /// (everything after it is rolled back). `steps` unapplies that many of
    /// the most recently applied visible migrations. Exactly one of the two
    /// must be set unless `steps` is `Some(n)`.
    ///
    /// # Errors
    /// Irreversible migrations, checksum mismatch, unknown target, backend
    /// errors.
    pub async fn rollback(
        &self,
        target: Option<&str>,
        steps: Option<u32>,
        dry_run: bool,
    ) -> Result<Report, MigrationError> {
        self.require_backend()?;
        if dry_run {
            let history = self.load_history().await?;
            self.verify_checksums(&history)?;
            let plan = self.rollback_plan(&history, target, steps)?;
            let sql = self.rollback_sql(&history, &plan)?;
            return Ok(Report {
                planned: plan,
                sql,
                applied: Vec::new(),
                dry_run: true,
            });
        }
        if self.db.is_scoped() {
            return Err(MigrationError::state(
                "migration execution requires a pool handle; \
                 caller-owned transactions and connections are unsupported",
            ));
        }
        self.with_lock(|session| {
            let this = self;
            async move {
                let created_history = if !this.history_table_exists_on(&session).await? {
                    session
                        .execute_script(&create_history_sql(this.kind()))
                        .await?;
                    true
                } else {
                    false
                };
                let history = this.load_history_from(&session).await?;
                this.verify_checksums(&history)?;
                let plan = this.rollback_plan(&history, target, steps)?;
                let mut sql = this.rollback_sql(&history, &plan)?;
                if created_history {
                    sql.insert(0, create_history_sql(this.kind()));
                }
                let mut unapplied = Vec::new();
                for id in &plan {
                    let migration = this.require_migration(id)?.clone();
                    let state_before = state_before_migration(this.graph, &history, id)?;
                    this.unapply_one(&session, &migration, &state_before)
                        .await?;
                    unapplied.push(id.clone());
                }
                Ok(Report {
                    planned: plan,
                    sql,
                    applied: unapplied,
                    dry_run: false,
                })
            }
        })
        .await
    }

    /// Every loaded migration with its applied flag (including replaced ids).
    ///
    /// # Errors
    /// History-table or checksum errors.
    pub async fn show(&self) -> Result<Vec<(String, bool)>, MigrationError> {
        let history = self.load_history().await?;
        self.verify_checksums(&history)?;
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        for id in &self.graph.order {
            if let Some(m) = self.graph.get(id) {
                rows.push((id.clone(), is_applied(m, &history.ids)));
                seen.insert(id.clone());
            }
        }
        let mut rest: Vec<&Migration> = self
            .graph
            .all
            .iter()
            .filter(|m| !seen.contains(&m.id))
            .collect();
        rest.sort_by(|a, b| a.id.cmp(&b.id));
        for m in rest {
            rows.push((m.id.clone(), is_applied(m, &history.ids)));
        }
        Ok(rows)
    }

    fn kind(&self) -> BackendKind {
        self.db.capabilities().kind
    }

    async fn with_lock<F, Fut>(&self, f: F) -> Result<Report, MigrationError>
    where
        F: FnOnce(Db) -> Fut,
        Fut: Future<Output = Result<Report, MigrationError>>,
    {
        // SQLite holds a single `BEGIN IMMEDIATE` transaction for the whole
        // run (the transaction *is* the lock): `migrate`/`rollback` there
        // are all-or-nothing, and the per-migration `atomic` flag has no
        // effect. PostgreSQL and MySQL hold a session lock on a
        // non-transactional connection instead, and each migration commits
        // on its own.
        let transactional = self.kind() == BackendKind::Sqlite;
        self.db
            .schema_change(transactional, |session| async move {
                locks::acquire(&session, self.lock_timeout).await?;
                let result = f(session.clone()).await;
                let unlocked = release_migrate_lock(&session).await;
                match (result, unlocked) {
                    (Ok(v), Ok(())) => Ok(v),
                    (Err(e), _) => Err(e),
                    // Work finished but ownership is unconfirmed; say so
                    // instead of surfacing a bare driver error.
                    (Ok(report), Err(e)) => Err(uncertain(report, e)),
                }
            })
            .await
    }

    fn require_backend(&self) -> Result<(), MigrationError> {
        match self.kind() {
            BackendKind::Postgres | BackendKind::Sqlite | BackendKind::MySql => Ok(()),
            other => Err(MigrationError::UnsupportedBackend(other)),
        }
    }

    fn require_migration(&self, id: &str) -> Result<&Migration, MigrationError> {
        self.graph
            .get(id)
            .ok_or_else(|| MigrationError::usage(format!("unknown migration `{id}`")))
    }

    /// Render `plan` to SQL, replaying project state from `history`.
    ///
    /// Returns the statements and the pre-plan state (callers feed it to
    /// `apply_one`, which advances it migration by migration so rendering
    /// and execution agree).
    fn plan_sql(
        &self,
        history: &History,
        plan: &[String],
    ) -> Result<(Vec<String>, ProjectState), MigrationError> {
        let state = replay(self.graph, history)?;
        let mut render = state.clone();
        let mut sql = Vec::new();
        if !history.table_exists {
            sql.push(create_history_sql(self.kind()));
        }
        for id in plan {
            let migration = self.require_migration(id)?;
            sql.extend(schema_editor::statements(
                self.kind(),
                &render,
                &migration.operations,
            )?);
            for op in &migration.operations {
                op.apply_to_state(&mut render)?;
            }
        }
        Ok((sql, state))
    }

    /// Render reverse SQL for `plan` from `history`.
    fn rollback_sql(
        &self,
        history: &History,
        plan: &[String],
    ) -> Result<Vec<String>, MigrationError> {
        let mut sql = Vec::new();
        for id in plan {
            let migration = self.require_migration(id)?;
            let state_before = state_before_migration(self.graph, history, id)?;
            sql.extend(reverse_sql(self.kind(), migration, &state_before)?);
        }
        Ok(sql)
    }

    fn forward_plan(
        &self,
        history: &History,
        target: Option<&str>,
    ) -> Result<Vec<String>, MigrationError> {
        if let Some(id) = target
            && self.graph.get(id).is_none()
        {
            return Err(MigrationError::usage(format!("unknown migration `{id}`")));
        }
        let mut plan = Vec::new();
        for id in &self.graph.order {
            let migration = self.require_migration(id)?;
            if is_applied(migration, &history.ids) {
                if target == Some(id.as_str()) {
                    return Ok(plan);
                }
                continue;
            }
            plan.push(id.clone());
            if target == Some(id.as_str()) {
                break;
            }
        }
        if let Some(id) = target
            && !plan.iter().any(|p| p == id)
            && !history.ids.contains(id)
            && !self
                .graph
                .get(id)
                .is_some_and(|m| is_applied(m, &history.ids))
        {
            // Target is a replaced id: applying the squash (already in plan or applied)
            // is enough; if the squash is not in the visible graph, error.
            if self.graph.replaced_by.contains_key(id) {
                return Ok(plan);
            }
            return Err(MigrationError::usage(format!(
                "migration `{id}` is not reachable from the current graph"
            )));
        }
        Ok(plan)
    }

    fn rollback_plan(
        &self,
        history: &History,
        target: Option<&str>,
        steps: Option<u32>,
    ) -> Result<Vec<String>, MigrationError> {
        let applied_visible: Vec<String> = self
            .graph
            .order
            .iter()
            .filter(|id| {
                self.graph
                    .get(id)
                    .is_some_and(|m| is_applied(m, &history.ids))
            })
            .cloned()
            .collect();
        if let Some(n) = steps {
            let n = n as usize;
            let start = applied_visible.len().saturating_sub(n);
            let mut plan = applied_visible[start..].to_vec();
            plan.reverse();
            return Ok(plan);
        }
        let Some(target) = target else {
            return Err(MigrationError::usage(
                "rollback requires --steps N or a target id",
            ));
        };
        if !applied_visible.iter().any(|id| id == target) && !history.ids.contains(target) {
            return Err(MigrationError::usage(format!(
                "migration `{target}` is not applied"
            )));
        }
        // Unapply everything after `target` (target stays applied).
        let mut plan = Vec::new();
        let mut after = false;
        for id in &applied_visible {
            if after {
                plan.push(id.clone());
            }
            if id == target {
                after = true;
            }
        }
        plan.reverse();
        Ok(plan)
    }

    fn verify_checksums(&self, history: &History) -> Result<(), MigrationError> {
        for migration in &self.graph.all {
            if let Some(stored) = history.checksums.get(&migration.id)
                && stored != &migration.checksum
            {
                return Err(MigrationError::ChecksumMismatch {
                    id: migration.id.clone(),
                    history: stored.clone(),
                    file: migration.checksum.clone(),
                });
            }
        }
        Ok(())
    }

    async fn load_history(&self) -> Result<History, MigrationError> {
        self.load_history_from(self.db).await
    }

    async fn load_history_from(&self, db: &Db) -> Result<History, MigrationError> {
        if !self.history_table_exists_on(db).await? {
            return Ok(History {
                ids: HashSet::new(),
                checksums: HashMap::new(),
                table_exists: false,
            });
        }
        // Any failure past this point (permissions, a dropped connection, a
        // damaged table) must surface: treating it as an empty history would
        // replay every migration against a live database.
        let result = db
            .raw_sql(
                &format!(
                    "SELECT {} as id, {} as checksum FROM {}",
                    quote_star(self.kind(), "id"),
                    quote_star(self.kind(), "checksum"),
                    quote_star(self.kind(), HISTORY_TABLE)
                ),
                Vec::new(),
            )
            .await?;
        let mut ids = HashSet::new();
        let mut checksums = HashMap::new();
        for row in result.rows {
            let Some(Value::Text(id)) = row.get("id") else {
                return Err(MigrationError::state(format!(
                    "`{HISTORY_TABLE}` has a row without a text `id`"
                )));
            };
            let checksum = match row.get("checksum") {
                Some(Value::Text(s)) => s.clone(),
                _ => String::new(),
            };
            ids.insert(id.clone());
            checksums.insert(id.clone(), checksum);
        }
        Ok(History {
            ids,
            checksums,
            table_exists: true,
        })
    }

    /// Ask the catalog whether the history table exists, so a failing read
    /// of the table itself is never mistaken for a fresh database.
    async fn history_table_exists_on(&self, db: &Db) -> Result<bool, MigrationError> {
        self.table_exists_on(db, HISTORY_TABLE).await
    }

    async fn table_exists_on(&self, db: &Db, table: &str) -> Result<bool, MigrationError> {
        catalog::table_exists(db, table).await
    }

    async fn apply_one(
        &self,
        db: &Db,
        migration: &Migration,
        state: &mut ProjectState,
    ) -> Result<(), MigrationError> {
        let kind = self.kind();
        let migration = migration.clone();
        let start_state = state.clone();
        let run = |db: Db| {
            let migration = migration.clone();
            let registry = self.registry.clone();
            let start_state = start_state.clone();
            async move {
                apply_ops_in_order(&db, kind, &migration, &start_state, &registry).await?;
                Ok::<_, MigrationError>(())
            }
        };
        // Migrations always run under `with_lock`. On SQLite that holds one
        // transaction for the whole run (the lock), so this branch always
        // hits; on other backends each migration gets its own transaction
        // when `runs_in_transaction`.
        if db.in_transaction() {
            run(db.clone()).await?;
        } else if runs_in_transaction(db, &migration) {
            db.transaction(run).await?;
        } else {
            run(db.clone()).await?;
        }
        for op in &migration.operations {
            op.apply_to_state(state)?;
        }
        Ok(())
    }

    async fn unapply_one(
        &self,
        db: &Db,
        migration: &Migration,
        state_before: &ProjectState,
    ) -> Result<(), MigrationError> {
        let kind = self.kind();
        // See `apply_one`: the whole migration travels into the run closure
        // because the reverse direction needs its id, checksum and `atomic`
        // flag for the progress row alongside its operations.
        let migration = migration.clone();
        let start_state = state_before.clone();
        let run = |db: Db| {
            let migration = migration.clone();
            let registry = self.registry.clone();
            let start_state = start_state.clone();
            async move {
                unapply_ops_in_order(&db, kind, &migration, &start_state, &registry).await?;
                Ok::<_, MigrationError>(())
            }
        };
        // See `apply_one`: SQLite always arrives inside the run-wide
        // transaction held by `with_lock`.
        if db.in_transaction() {
            run(db.clone()).await?;
        } else if runs_in_transaction(db, &migration) {
            db.transaction(run).await?;
        } else {
            run(db.clone()).await?;
        }
        Ok(())
    }
}

struct History {
    ids: HashSet<String>,
    checksums: HashMap<String, String>,
    table_exists: bool,
}

fn quote_star(kind: BackendKind, ident: &str) -> String {
    schema_editor::quote_identifier(kind, ident)
}

/// Whether one migration can be wrapped in a transaction that also undoes
/// its DDL. MySQL commits implicitly on every DDL statement, so wrapping it
/// would only suggest an atomicity the database cannot give.
fn runs_in_transaction(db: &Db, migration: &Migration) -> bool {
    let caps = db.capabilities();
    migration.atomic
        && caps.transactions > TransactionSupport::None
        && caps.kind != BackendKind::MySql
}

/// What a failed migration leaves behind on the server.
///
/// Decides both whether the executor records per-statement progress (only a
/// run whose earlier statements survive a failure can be resumed) and what a
/// failure reports: MySQL commits DDL implicitly but rolls DML back with the
/// surrounding statement, while a PostgreSQL migration with `atomic: false`
/// runs outside a transaction, where *every* statement autocommits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnFailure {
    /// A transaction wraps the run: a failure rolls every statement back.
    Rollback,
    /// MySQL: only implicit-commit DDL survives a failure.
    ImplicitCommit,
    /// PostgreSQL with `atomic: false`: no transaction, so every statement
    /// autocommits (DML included).
    Autocommit,
}

impl OnFailure {
    /// Partial-failure behaviour of one migration on `kind`.
    ///
    /// MySQL is always partial, whatever `atomic` says. PostgreSQL is partial
    /// only without the transaction, i.e. when the migration opted out with
    /// `atomic: false` (what `CREATE INDEX CONCURRENTLY` needs). SQLite never
    /// is: the whole `migrate` run is the lock's transaction.
    fn of(kind: BackendKind, atomic: bool) -> Self {
        match kind {
            BackendKind::MySql => Self::ImplicitCommit,
            BackendKind::Postgres if !atomic => Self::Autocommit,
            _ => Self::Rollback,
        }
    }

    /// Whether per-statement progress is recorded for this kind of run.
    const fn resumes(self) -> bool {
        !matches!(self, Self::Rollback)
    }

    /// Whether an earlier statement of this run is committed for good.
    ///
    /// MySQL counts only implicit-commit DDL, so a failure after mere DML
    /// still reports the plain error; a non-transactional PostgreSQL run
    /// counts anything, because outside a transaction every statement
    /// autocommits.
    fn prior_committed(self, committed: bool, ddl: bool) -> bool {
        match self {
            Self::Rollback => false,
            Self::ImplicitCommit => ddl,
            Self::Autocommit => committed,
        }
    }
}

/// [`MigrationError`] for a `RunRust` failure after earlier statements
/// committed. The operation's own error is kept as the source.
fn op_partial(
    failure: OnFailure,
    id: &str,
    index: usize,
    total: usize,
    summary: String,
    source: MigrationError,
) -> MigrationError {
    let id = id.to_owned();
    match failure {
        OnFailure::ImplicitCommit => MigrationError::MysqlOpPartial {
            id,
            index,
            total,
            summary,
            source: Box::new(source),
        },
        OnFailure::Autocommit => MigrationError::PostgresOpPartial {
            id,
            index,
            total,
            summary,
            source: Box::new(source),
        },
        OnFailure::Rollback => source,
    }
}

fn create_history_sql(kind: BackendKind) -> String {
    // MySQL cannot key a TEXT column without a prefix length.
    let key_type = if kind == BackendKind::MySql {
        "VARCHAR(255)"
    } else {
        "TEXT"
    };
    format!(
        "CREATE TABLE IF NOT EXISTS {} (\n  {} {key_type} PRIMARY KEY,\n  {} TEXT NOT NULL,\n  {} TEXT NOT NULL\n)",
        quote_star(kind, HISTORY_TABLE),
        quote_star(kind, "id"),
        quote_star(kind, "checksum"),
        quote_star(kind, "applied_at"),
    )
}

async fn execute_one_sql(
    db: &Db,
    failure: OnFailure,
    migration_id: &str,
    index: usize,
    total: usize,
    prior_committed: bool,
    sql: &str,
) -> Result<(), MigrationError> {
    if let Err(err) = db.execute_script(sql).await {
        // With earlier statements committed for good, a re-run replays them
        // and gets stuck, so name the failed statement and what is left
        // behind. Without them (a first-statement failure, or only rolled-back
        // DML before it) the plain error is accurate.
        if prior_committed {
            return Err(match failure {
                OnFailure::ImplicitCommit => MigrationError::MysqlPartial {
                    id: migration_id.to_owned(),
                    index,
                    total,
                    source: err,
                },
                OnFailure::Autocommit => MigrationError::PostgresPartial {
                    id: migration_id.to_owned(),
                    index,
                    total,
                    source: err,
                },
                OnFailure::Rollback => err.into(),
            });
        }
        return Err(err.into());
    }
    Ok(())
}

/// Whether `sql` is (probably) DDL that MySQL commits implicitly, so a
/// later failure leaves it applied. Matches the statement's first keyword,
/// skipping whitespace and `--` / `/* */` comments.
fn is_implicit_commit(sql: &str) -> bool {
    matches!(
        first_keyword(sql).as_str(),
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "RENAME"
    )
}

fn first_keyword(sql: &str) -> String {
    let mut rest = sql.trim_start();
    loop {
        if let Some(body) = rest.strip_prefix("--") {
            rest = body
                .split_once('\n')
                .map(|(_, tail)| tail)
                .unwrap_or("")
                .trim_start();
        } else if let Some(body) = rest.strip_prefix("/*") {
            rest = body
                .split_once("*/")
                .map(|(_, tail)| tail)
                .unwrap_or("")
                .trim_start();
        } else {
            break;
        }
    }
    rest.split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
}

fn placeholder(kind: BackendKind, index: usize) -> String {
    match kind {
        BackendKind::Postgres => format!("${index}"),
        _ => "?".to_owned(),
    }
}

async fn record_history(
    db: &Db,
    kind: BackendKind,
    migration: &Migration,
) -> Result<(), MigrationError> {
    let sql = format!(
        "INSERT INTO {} ({}, {}, {}) VALUES ({}, {}, {})",
        quote_star(kind, HISTORY_TABLE),
        quote_star(kind, "id"),
        quote_star(kind, "checksum"),
        quote_star(kind, "applied_at"),
        placeholder(kind, 1),
        placeholder(kind, 2),
        placeholder(kind, 3),
    );
    let applied_at = Utc::now().format(TIMESTAMP_FORMAT).to_string();
    db.raw_execute(
        &sql,
        vec![
            Value::Text(migration.id.clone()),
            Value::Text(migration.checksum.clone()),
            Value::Text(applied_at),
        ],
    )
    .await?;
    Ok(())
}

async fn delete_history(db: &Db, kind: BackendKind, id: &str) -> Result<(), MigrationError> {
    let sql = format!(
        "DELETE FROM {} WHERE {} = {}",
        quote_star(kind, HISTORY_TABLE),
        quote_star(kind, "id"),
        placeholder(kind, 1),
    );
    db.raw_execute(&sql, vec![Value::Text(id.to_owned())])
        .await?;
    Ok(())
}

fn replay(graph: &MigrationGraph, history: &History) -> Result<ProjectState, MigrationError> {
    let mut state = ProjectState::new();
    for id in &graph.order {
        let Some(migration) = graph.get(id) else {
            continue;
        };
        if !is_applied(migration, &history.ids) {
            continue;
        }
        for op in &migration.operations {
            op.apply_to_state(&mut state)?;
        }
    }
    Ok(state)
}

fn state_before_migration(
    graph: &MigrationGraph,
    history: &History,
    id: &str,
) -> Result<ProjectState, MigrationError> {
    let mut state = ProjectState::new();
    for vis in &graph.order {
        if vis == id {
            break;
        }
        let Some(migration) = graph.get(vis) else {
            continue;
        };
        if !is_applied(migration, &history.ids) {
            continue;
        }
        for op in &migration.operations {
            op.apply_to_state(&mut state)?;
        }
    }
    Ok(state)
}

fn reverse_sql(
    kind: BackendKind,
    migration: &Migration,
    state_before: &ProjectState,
) -> Result<Vec<String>, MigrationError> {
    let mut snapshots = vec![state_before.clone()];
    let mut current = state_before.clone();
    for op in &migration.operations {
        op.apply_to_state(&mut current)?;
        snapshots.push(current.clone());
    }
    let mut sql = Vec::new();
    current = snapshots
        .last()
        .cloned()
        .unwrap_or_else(|| state_before.clone());
    for i in (0..migration.operations.len()).rev() {
        let op = &migration.operations[i];
        let Some(rev) = op.reverse(&snapshots[i]) else {
            return Err(MigrationError::Irreversible {
                id: migration.id.clone(),
                reason: format!("{} has no reverse", op.summary()),
            });
        };
        sql.extend(schema_editor::render(kind, &rev, &current)?);
        rev.apply_to_state(&mut current)?;
    }
    Ok(sql)
}

fn uncertain(report: Report, source: MigrationError) -> MigrationError {
    MigrationError::LockReleaseUncertain {
        applied: report.applied,
        source: Box::new(source),
    }
}
