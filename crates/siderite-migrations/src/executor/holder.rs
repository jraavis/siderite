//! Read-only identification of the session holding the migration lock.
//!
//! Only the lock-timeout error path calls this: successful runs never pay
//! for the catalog reads.

use super::locks::{MYSQL_LOCK_NAME, PG_MIGRATE_LOCK};
use super::{BackendKind, MigrationError as Error, Migrator, Value};
use serde::Serialize;
use siderite_orm::Row;
use std::fmt;

/// The session that holds the migration lock, as far as the server shows
/// it to the inspecting role. Fields other than `session` are `None` when
/// the server hides them (PostgreSQL without `pg_read_all_stats`, MySQL
/// without `PROCESS`) or the session ended between the two reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LockHolder {
    /// PostgreSQL backend pid or MySQL connection id.
    pub session: i64,
    /// Login role (PostgreSQL) or user (MySQL).
    pub user: Option<String>,
    /// Client address (PostgreSQL) or `host:port` (MySQL).
    pub client: Option<String>,
    /// PostgreSQL `application_name`; MySQL `COMMAND`.
    pub application: Option<String>,
    /// Session state (`active`, `idle in transaction`, MySQL `STATE`).
    pub state: Option<String>,
    /// Seconds in the current state (PostgreSQL) or command (MySQL).
    pub seconds: Option<i64>,
}

impl fmt::Display for LockHolder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "session {}", self.session)?;
        let parts = [
            ("user", &self.user),
            ("client", &self.client),
            ("application", &self.application),
            ("state", &self.state),
        ];
        for (label, value) in parts {
            if let Some(value) = value {
                write!(f, ", {label} {value}")?;
            }
        }
        if let Some(seconds) = self.seconds {
            write!(f, ", for {seconds}s")?;
        }
        Ok(())
    }
}

/// Advisory locks taken with one `bigint` key store its high and low
/// halves in `classid` / `objid` and `objsubid = 1`.
const PG_HOLDER: &str = "SELECT l.pid::bigint AS session, \
    a.usename::text AS usr, a.client_addr::text AS client, \
    a.application_name::text AS app, a.state::text AS state, \
    CAST(EXTRACT(EPOCH FROM now() - a.state_change) AS bigint) AS seconds \
    FROM pg_catalog.pg_locks l \
    LEFT JOIN pg_catalog.pg_stat_activity a ON a.pid = l.pid \
    WHERE l.locktype = 'advisory' AND l.granted AND l.objsubid = 1 \
    AND l.classid::bigint = $1 AND l.objid::bigint = $2 \
    AND l.database = (SELECT oid FROM pg_catalog.pg_database \
    WHERE datname = current_database()) LIMIT 1";

const MYSQL_SESSION: &str = "SELECT CAST(IS_USED_LOCK(?) AS SIGNED) \
    AS session";

const MYSQL_DETAILS: &str = "SELECT CAST(USER AS CHAR) AS usr, \
    CAST(HOST AS CHAR) AS client, CAST(COMMAND AS CHAR) AS app, \
    CAST(STATE AS CHAR) AS state, CAST(TIME AS SIGNED) AS seconds \
    FROM information_schema.processlist WHERE ID = ?";

impl Migrator<'_> {
    /// Identify the session currently holding the migration lock.
    ///
    /// Intended for the [`MigrationError::LockTimeout`](Error::LockTimeout)
    /// path: it takes no lock and changes nothing, and the holder may
    /// finish between the timeout and this read.
    ///
    /// Returns:
    ///   The holder, or `None` when the lock is free (or on SQLite, which
    ///   has no migration lock).
    ///
    /// # Errors
    /// Unsupported backend or unreadable catalogs.
    pub async fn lock_holder(&self) -> Result<Option<LockHolder>, Error> {
        match self.kind() {
            BackendKind::Sqlite => Ok(None),
            BackendKind::Postgres => self.pg_holder().await,
            BackendKind::MySql => self.mysql_holder().await,
            other => Err(Error::UnsupportedBackend(other)),
        }
    }

    async fn pg_holder(&self) -> Result<Option<LockHolder>, Error> {
        let params = vec![
            Value::Int(PG_MIGRATE_LOCK >> 32),
            Value::Int(PG_MIGRATE_LOCK & 0xffff_ffff),
        ];
        let rows = self.db.raw_sql(PG_HOLDER, params).await?.rows;
        Ok(rows.first().and_then(|row| {
            let session = int(row, "session")?;
            Some(details(session, Some(row)))
        }))
    }

    async fn mysql_holder(&self) -> Result<Option<LockHolder>, Error> {
        let name = vec![Value::Text(MYSQL_LOCK_NAME.into())];
        let rows = self.db.raw_sql(MYSQL_SESSION, name).await?.rows;
        let Some(session) = rows.first().and_then(|r| int(r, "session")) else {
            return Ok(None);
        };
        let id = vec![Value::Int(session)];
        let rows = self.db.raw_sql(MYSQL_DETAILS, id).await?.rows;
        Ok(Some(details(session, rows.first())))
    }
}

fn details(session: i64, row: Option<&Row>) -> LockHolder {
    let text = |key| row.and_then(|row| text(row, key));
    LockHolder {
        session,
        user: text("usr"),
        client: text("client"),
        application: text("app"),
        state: text("state"),
        seconds: row.and_then(|row| int(row, "seconds")),
    }
}

fn int(row: &Row, key: &str) -> Option<i64> {
    match row.get(key) {
        Some(Value::Int(n)) => Some(*n),
        _ => None,
    }
}

fn text(row: &Row, key: &str) -> Option<String> {
    match row.get(key) {
        Some(Value::Text(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::LockHolder;

    #[test]
    fn display_lists_visible_fields_only() {
        let mut holder = LockHolder {
            session: 42,
            user: Some("app".into()),
            client: None,
            application: Some("deploy".into()),
            state: Some("active".into()),
            seconds: Some(7),
        };
        assert_eq!(
            holder.to_string(),
            "session 42, user app, application deploy, state active, for 7s"
        );
        holder = LockHolder {
            user: None,
            application: None,
            state: None,
            seconds: None,
            ..holder
        };
        assert_eq!(holder.to_string(), "session 42");
    }
}
