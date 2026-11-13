//! Plan execution, statement semantics and connection hand-back.

use super::*;
use siderite_orm::expr::Field;
use siderite_orm::{BackendCapabilityError, Expr, LockMode, OrderDirection};

struct User;

const NAME: Field<User, String> = Field::new("name");
const AGE: Field<User, i64> = Field::new("age");

type Check = Result<(), OrmError>;

async fn seeded(options: NativeSqliteOptions) -> Result<NativeSqliteBackend, OrmError> {
    let db = NativeSqliteBackend::connect_with("sqlite::memory:", options).await?;
    db.execute_script(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER);
         INSERT INTO users (name, age) VALUES ('Alice', 30), ('bob', NULL), ('ALINA', 22), ('50%', 1);",
    )
    .await?;
    Ok(db)
}

fn names(r: &QueryResult) -> Vec<String> {
    r.rows
        .iter()
        .map(|row| match row.get("name") {
            Some(Value::Text(s)) => s.clone(),
            other => panic!("unexpected {other:?}"),
        })
        .collect()
}

fn every_dispatch() -> [NativeSqliteOptions; 2] {
    let inline = NativeSqliteOptions {
        reads: Dispatch::Inline,
        writes: Dispatch::Inline,
        ..NativeSqliteOptions::default()
    };
    let blocking = NativeSqliteOptions {
        reads: Dispatch::Blocking,
        writes: Dispatch::Blocking,
        ..NativeSqliteOptions::default()
    };
    [inline, blocking]
}

#[tokio::test]
async fn executes_filters_ordering_and_limits() -> Check {
    for options in every_dispatch() {
        let db = seeded(options).await?;
        let plan = QueryPlan::from_table("users")
            .filter(NAME.icontains("al"))
            .order_by(AGE, OrderDirection::Asc);
        assert_eq!(names(&db.fetch(&plan).await?), ["ALINA", "Alice"]);

        let nulls = QueryPlan::from_table("users").filter(AGE.is_null());
        assert_eq!(names(&db.fetch(&nulls).await?), ["bob"]);

        let wildcard = QueryPlan::from_table("users").filter(NAME.icontains("%"));
        assert_eq!(names(&db.fetch(&wildcard).await?), ["50%"]);

        let page = QueryPlan::from_table("users")
            .order_by(Expr::col("id"), OrderDirection::Asc)
            .offset(3);
        assert_eq!(names(&db.fetch(&page).await?), ["50%"]);
    }
    Ok(())
}

#[tokio::test]
async fn row_locking_is_rejected_before_io() -> Check {
    let db = seeded(NativeSqliteOptions::default()).await?;
    let err = db
        .fetch(&QueryPlan::from_table("users").lock(LockMode::ForUpdate))
        .await;
    assert!(matches!(
        err,
        Err(OrmError::Capability(
            BackendCapabilityError::RowLockingUnsupported { .. }
        ))
    ));
    Ok(())
}

#[tokio::test]
async fn several_statements_number_parameters_across_them() -> Check {
    let db = seeded(NativeSqliteOptions::default()).await?;
    let changed = db
        .execute_raw(
            "UPDATE users SET age = ? WHERE name = ?; UPDATE users SET age = ?2 WHERE name = 'bob'",
            vec![Value::Int(31), Value::Text("Alice".into())],
        )
        .await?;
    assert_eq!(changed, 2);
    let rows = db
        .fetch_raw(
            "SELECT age FROM users WHERE name = ?; SELECT ? AS age, ? AS missing",
            vec![Value::Text("Alice".into()), Value::Int(7)],
        )
        .await?
        .rows;
    // Every row shares the first statement's header; surplus values are
    // ignored and missing ones bind NULL.
    let ages: Vec<_> = rows.iter().map(|row| row.get("age").cloned()).collect();
    assert_eq!(ages, [Some(Value::Int(31)), Some(Value::Int(7))]);
    assert_eq!(rows[1].len(), 1);
    let unbound = db
        .fetch_raw("SELECT ? AS a, ? AS b", vec![Value::Int(1)])
        .await?;
    assert_eq!(unbound.rows[0].get("b"), Some(&Value::Null));
    let ignored = db
        .fetch_raw("SELECT ? AS a", vec![Value::Int(1), Value::Int(2)])
        .await?;
    assert_eq!(ignored.rows[0].get("a"), Some(&Value::Int(1)));
    Ok(())
}

#[tokio::test]
async fn empty_and_comment_only_sql_do_nothing() -> Check {
    let db = seeded(NativeSqliteOptions::default()).await?;
    assert!(db.fetch_raw("", Vec::new()).await?.rows.is_empty());
    assert!(
        db.fetch_raw("-- nothing", Vec::new())
            .await?
            .rows
            .is_empty()
    );
    db.execute_script("/* nothing */").await?;
    Ok(())
}

/// A cached statement keeps no binding from its previous run.
#[tokio::test]
async fn cached_statements_start_unbound() -> Check {
    let db = seeded(NativeSqliteOptions::default()).await?;
    let sql = "SELECT ? AS a, ? AS b";
    db.fetch_raw(sql, vec![Value::Int(1), Value::Int(2)])
        .await?;
    let again = db.fetch_raw(sql, vec![Value::Int(3)]).await?;
    assert_eq!(again.rows[0].get("b"), Some(&Value::Null));
    Ok(())
}

#[tokio::test]
async fn dropped_lease_rolls_back_and_returns_its_connection() -> Check {
    for options in every_dispatch() {
        let db = seeded(options).await?;
        let lease = db.writer().await?;
        exec::script(lease.conn()?, "BEGIN; DELETE FROM users", Vec::new())?;
        drop(lease);
        // Offloaded resets finish on the blocking pool; the next checkout
        // waits for the connection to come back.
        let lease = db.writer().await?;
        assert!(lease.conn()?.is_autocommit());
        drop(lease);
        let count = db.fetch_raw("SELECT COUNT(*) AS n FROM users", Vec::new());
        assert_eq!(count.await?.rows[0].get("n"), Some(&Value::Int(4)));
    }
    Ok(())
}

#[tokio::test]
async fn readers_wait_for_a_free_connection_then_time_out() -> Check {
    let file = std::env::temp_dir().join(format!("siderite-pool-{}.db", std::process::id()));
    let url = format!("sqlite://{}?mode=rwc", file.display());
    let options = NativeSqliteOptions {
        readers: 1,
        acquire_timeout: Duration::from_millis(50),
        ..NativeSqliteOptions::default()
    };
    let db = NativeSqliteBackend::connect_with(&url, options).await?;
    let held = db.reader().await?;
    let waited = db.fetch_raw("SELECT 1", Vec::new()).await;
    let message = waited.map(drop).map_err(|e| e.to_string());
    assert_eq!(
        message,
        Err("database error: pool timed out while waiting for an open connection".into())
    );
    let waiting = tokio::spawn({
        let db = db.clone();
        async move { db.fetch_raw("SELECT 2 AS n", Vec::new()).await }
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    drop(held);
    let rows = waiting
        .await
        .map_err(|e| BackendError::Database(e.to_string()))??;
    assert_eq!(rows.rows[0].get("n"), Some(&Value::Int(2)));
    drop(db);
    for suffix in ["", "-journal", "-wal", "-shm"] {
        drop(std::fs::remove_file(format!("{}{suffix}", file.display())));
    }
    Ok(())
}

#[tokio::test]
async fn invalid_settings_fail_to_connect() {
    let none = NativeSqliteOptions {
        readers: 0,
        ..NativeSqliteOptions::default()
    };
    let result = NativeSqliteBackend::connect_with("sqlite::memory:", none).await;
    assert!(matches!(result, Err(BackendError::Connection(_))));
    let missing = NativeSqliteBackend::connect("sqlite:///nonexistent/dir/app.db").await;
    assert!(matches!(missing, Err(BackendError::Connection(_))));
}
