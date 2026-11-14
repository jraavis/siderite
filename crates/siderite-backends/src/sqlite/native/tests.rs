//! Plan execution, statement semantics and connection hand-back.

use super::*;
use siderite_orm::expr::Field;
use siderite_orm::{BackendCapabilityError, Cells, Expr, LockMode, OrderDirection};
use siderite_orm::{QueryError, Row, RowSink};

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

/// Takes the `take` columns of every row by index, or the whole row when
/// `take` is empty, and fails on row `fail_at`.
#[derive(Default)]
struct Probe {
    take: Vec<usize>,
    fail_at: Option<usize>,
    taken: Vec<Vec<Option<Value>>>,
    rows: Vec<Row>,
}

impl RowSink for Probe {
    fn push(&mut self, cells: &mut dyn Cells) -> Result<(), QueryError> {
        if self.fail_at == Some(self.rows.len() + self.taken.len()) {
            return Err(QueryError::Model("rejected".into()));
        }
        if self.take.is_empty() {
            self.rows.push(cells.to_row()?);
        } else {
            let taken = self.take.iter().map(|&index| {
                let mut value = Value::Null;
                Ok(cells.take(index, &mut value)?.then_some(value))
            });
            self.taken.push(taken.collect::<Result<_, _>>()?);
        }
        Ok(())
    }
}

fn by_id() -> QueryPlan {
    QueryPlan::from_table("users").order_by(Field::<User, i64>::new("id"), OrderDirection::Asc)
}

#[tokio::test]
async fn fetch_each_hands_over_the_rows_fetch_returns() -> Check {
    for options in every_dispatch() {
        let db = seeded(options).await?;
        let fetched = db.fetch(&by_id()).await?.rows;
        let mut whole = Probe::default();
        db.fetch_each(&by_id(), &mut whole).await?;
        assert_eq!(whole.rows, fetched);

        let mut cells = Probe {
            take: vec![1, 2, 9],
            ..Probe::default()
        };
        db.fetch_each(&by_id(), &mut cells).await?;
        let expected: Vec<_> = fetched
            .iter()
            .map(|row| vec![row.get("name").cloned(), row.get("age").cloned(), None])
            .collect();
        assert_eq!(cells.taken, expected);

        let tx = db.begin(None).await?;
        let mut held = Probe::default();
        tx.fetch_each(&by_id(), &mut held).await?;
        assert_eq!(held.rows, fetched);
        tx.rollback().await?;
    }
    Ok(())
}

#[tokio::test]
async fn fetch_each_reports_the_first_rejection_and_cell_errors() -> Check {
    for options in every_dispatch() {
        let inline = options.reads == Dispatch::Inline;
        let db = seeded(options).await?;
        let mut rejecting = Probe {
            fail_at: Some(1),
            ..Probe::default()
        };
        let rejected = db.fetch_each(&by_id(), &mut rejecting).await;
        assert_eq!(rejected.map_err(|e| e.to_string()), Err("rejected".into()));
        assert_eq!(rejecting.rows.len(), 1);
        assert_eq!(db.fetch(&by_id()).await?.rows.len(), 4);

        db.execute_raw(
            "INSERT INTO users (name, age) VALUES (CAST(x'ff' AS TEXT), 99)",
            Vec::new(),
        )
        .await?;
        let bad = by_id().filter(AGE.eq(99_i64));
        let fetched = db.fetch(&bad).await.map(drop).map_err(|e| e.to_string());
        let mut name = Probe {
            take: vec![1],
            ..Probe::default()
        };
        let each = db
            .fetch_each(&bad, &mut name)
            .await
            .map_err(|e| e.to_string());
        assert!(fetched.as_ref().is_err_and(|e| e.contains("`name`")));
        assert_eq!(each, fetched);
        // Inline, a cell nobody reads is never decoded.
        let mut age = Probe {
            take: vec![2],
            ..Probe::default()
        };
        if inline {
            db.fetch_each(&bad, &mut age).await?;
            assert_eq!(age.taken, [[Some(Value::Int(99))]]);
        }
    }
    Ok(())
}
