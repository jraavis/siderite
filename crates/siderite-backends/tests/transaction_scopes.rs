//! Transaction scope ownership against a real SQLite connection.
#![cfg(any(feature = "sqlite", feature = "postgres", feature = "mysql"))]

#[cfg(feature = "sqlite")]
#[path = "common/backend.rs"]
mod backend;

#[cfg(feature = "sqlite")]
use backend::TestSqlite;
use siderite_orm::{BackendKind, Db, OrmError, QueryError, Value};
use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::oneshot;

#[cfg(feature = "sqlite")]
async fn fixture() -> Result<Db, OrmError> {
    let db = Db::new(TestSqlite::connect("sqlite::memory:").await?);
    let sql = "CREATE TABLE siderite_scope_writes (id INTEGER PRIMARY KEY)";
    db.execute_script(sql).await?;
    Ok(db)
}

async fn write(db: &Db, id: i64) -> Result<(), OrmError> {
    let sql = if db.capabilities().kind == BackendKind::Postgres {
        "INSERT INTO siderite_scope_writes (id) VALUES ($1)"
    } else {
        "INSERT INTO siderite_scope_writes (id) VALUES (?)"
    };
    db.raw_execute(sql, vec![Value::Int(id)]).await?;
    Ok(())
}

async fn assert_count(db: &Db, expected: i64) -> Result<(), OrmError> {
    let result = db
        .raw_sql("SELECT COUNT(*) AS n FROM siderite_scope_writes", vec![])
        .await?;
    assert_eq!(result.rows[0].get("n"), Some(&Value::Int(expected)));
    Ok(())
}

async fn cancel_child(db: Db) -> Result<(), OrmError> {
    let hooks = Arc::new(AtomicUsize::new(0));
    let registered = Arc::clone(&hooks);
    let child_hooks = Arc::clone(&hooks);
    let result = db
        .transaction(|tx| async move {
            write(&tx, 1).await?;
            let captured = tx.clone();
            tx.on_commit(move || {
                drop(captured);
                registered.fetch_add(1, Ordering::SeqCst);
            });
            let (written, wait_for_write) = oneshot::channel();
            let mut nested = Box::pin(tx.transaction(|child| async move {
                write(&child, 2).await?;
                let captured = child.clone();
                child.on_commit(move || {
                    drop((captured, child_hooks));
                    panic!("cancelled child's hook ran");
                });
                let _ = written.send(());
                pending::<Result<(), OrmError>>().await
            }));
            tokio::select! {
                _ = wait_for_write => {},
                result = &mut nested => panic!("child completed: {result:?}"),
            }
            drop(nested);
            assert!(matches!(
                write(&tx, 3).await,
                Err(OrmError::Query(QueryError::TransactionAborted))
            ));
            // Returning success after cancellation cannot commit writes.
            Ok::<_, OrmError>(())
        })
        .await;
    assert!(matches!(
        result,
        Err(OrmError::Query(QueryError::TransactionAborted))
    ));
    assert_eq!(hooks.load(Ordering::SeqCst), 0);
    assert_eq!(Arc::strong_count(&hooks), 1);
    assert_count(&db, 0).await?;
    db.transaction(|tx| async move { write(&tx, 4).await })
        .await?;
    assert_count(&db, 1).await
}

async fn scope_ownership(db: Db) -> Result<(), OrmError> {
    db.transaction(|tx| async move {
        let (started, ready) = oneshot::channel();
        let (finish, waiting) = oneshot::channel();
        let mut first = Box::pin(tx.transaction(|child| async move {
            write(&child, 1).await?;
            let _ = started.send(());
            let _ = waiting.await;
            Ok::<_, OrmError>(())
        }));
        tokio::select! {
            _ = ready => {},
            result = &mut first => panic!("child completed: {result:?}"),
        }
        let sibling = tx
            .transaction(|child| async move { write(&child, 2).await })
            .await;
        assert!(matches!(
            sibling,
            Err(OrmError::Query(QueryError::TransactionBusy))
        ));
        assert!(matches!(
            write(&tx, 3).await,
            Err(OrmError::Query(QueryError::TransactionBusy))
        ));
        let _ = finish.send(());
        first.await?;
        write(&tx, 4).await?;
        Ok::<_, OrmError>(())
    })
    .await?;
    assert_count(&db, 2).await
}

async fn recursive(db: Db) -> Result<(), OrmError> {
    let hooks = Arc::new(AtomicUsize::new(0));
    let registered = Arc::clone(&hooks);
    db.transaction(|tx| async move {
        tx.transaction(|child| async move {
            write(&child, 1).await?;
            child
                .transaction(|grandchild| async move {
                    write(&grandchild, 2).await?;
                    grandchild.on_commit(move || {
                        registered.fetch_add(1, Ordering::SeqCst);
                    });
                    Ok::<_, OrmError>(())
                })
                .await?;
            let failure = child
                .transaction(|failed| async move {
                    write(&failed, 3).await?;
                    failed.on_commit(|| panic!("rolled-back hook ran"));
                    let error = QueryError::InvalidPlan("stop".into());
                    Err::<(), OrmError>(error.into())
                })
                .await;
            assert!(failure.is_err());
            // Repeated failed children leave no abandoned savepoint scope.
            child
                .transaction(|next| async move { write(&next, 4).await })
                .await?;
            Ok::<_, OrmError>(())
        })
        .await
    })
    .await?;
    assert_count(&db, 3).await?;
    assert_eq!(hooks.load(Ordering::SeqCst), 1);
    Ok(())
}

async fn escaped(db: Db) -> Result<(), OrmError> {
    let outer = db
        .transaction(|tx| async move {
            let child = tx
                .transaction(|child| async move {
                    write(&child, 1).await?;
                    Ok::<_, OrmError>(child)
                })
                .await?;
            assert!(write(&child, 2).await.is_err());
            child.on_commit(|| panic!("escaped child's hook ran"));
            Ok::<_, OrmError>(tx)
        })
        .await?;
    assert!(matches!(
        write(&outer, 3).await,
        Err(OrmError::Query(QueryError::TransactionClosed))
    ));
    outer.on_commit(|| panic!("escaped outer hook ran"));
    assert_count(&db, 1).await
}

async fn cancel_outer(db: Db) -> Result<(), OrmError> {
    let (escaped, receiver) = oneshot::channel();
    let mut transaction = Box::pin(db.transaction(|tx| async move {
        write(&tx, 1).await?;
        let _ = escaped.send(tx.clone());
        pending::<Result<(), OrmError>>().await
    }));
    let retained = tokio::select! {
        result = receiver => {
            result.map_err(|e| QueryError::Model(e.to_string()))?
        },
        result = &mut transaction => panic!("transaction: {result:?}"),
    };
    drop(transaction);
    assert!(matches!(
        write(&retained, 2).await,
        Err(OrmError::Query(QueryError::TransactionAborted))
    ));
    assert_count(&db, 0).await
}

async fn panic_child(db: Db) -> Result<(), OrmError> {
    use futures_util::FutureExt;
    use std::panic::AssertUnwindSafe;

    fn child_panic() -> Result<(), OrmError> {
        panic!("child panic")
    }

    let result = db
        .transaction(|tx| async move {
            let panic = AssertUnwindSafe(tx.transaction(|child| async move {
                write(&child, 1).await?;
                child_panic()
            }))
            .catch_unwind()
            .await;
            assert!(panic.is_err());
            Ok::<_, OrmError>(())
        })
        .await;
    assert!(matches!(
        result,
        Err(OrmError::Query(QueryError::TransactionAborted))
    ));
    assert_count(&db, 0).await
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use super::*;

    macro_rules! cases {
        ($($name:ident),* $(,)?) => {$(
            #[tokio::test]
            async fn $name() -> Result<(), OrmError> {
                super::$name(fixture().await?).await
            }
        )*};
    }
    cases!(
        cancel_child,
        scope_ownership,
        recursive,
        escaped,
        cancel_outer,
        panic_child
    );
}

#[cfg(any(feature = "postgres", feature = "mysql"))]
async fn exercise_live(db: Db) -> Result<(), OrmError> {
    let sql = "CREATE TABLE siderite_scope_writes (id INTEGER PRIMARY KEY)";
    db.execute_script(sql).await?;
    macro_rules! cases {
        ($($name:ident),* $(,)?) => {$(
            db.execute_script("DELETE FROM siderite_scope_writes").await?;
            $name(db.clone()).await?;
        )*};
    }
    cases!(
        cancel_child,
        scope_ownership,
        recursive,
        escaped,
        cancel_outer,
        panic_child
    );
    Ok(())
}

#[cfg(any(feature = "postgres", feature = "mysql"))]
fn driver_error(error: sqlx::Error) -> OrmError {
    siderite_orm::BackendError::Database(error.to_string()).into()
}

#[cfg(any(feature = "postgres", feature = "mysql"))]
fn required_url(variable: &str) -> Result<String, OrmError> {
    std::env::var(variable).map_err(|_| {
        let message = format!("{variable} is required");
        let error = siderite_orm::BackendError::Connection(message);
        error.into()
    })
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "requires DATABASE_URL with CREATE DATABASE"]
async fn postgres_scope_contract() -> Result<(), OrmError> {
    use siderite_backends::postgres::PgBackend;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    let url = required_url("DATABASE_URL")?;
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .map_err(driver_error)?;
    let name = format!("siderite_scopes_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .map_err(driver_error)?;
    let options = url.parse::<PgConnectOptions>().map_err(driver_error)?;
    let result = async {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.database(&name))
            .await
            .map_err(driver_error)?;
        let db = Db::new(PgBackend::from_pool(pool.clone()));
        let result = exercise_live(db).await;
        pool.close().await;
        result
    }
    .await;
    let cleanup = sqlx::query(&format!("DROP DATABASE {name}"))
        .execute(&admin)
        .await
        .map_err(driver_error);
    admin.close().await;
    result?;
    cleanup?;
    Ok(())
}

#[cfg(feature = "mysql")]
#[tokio::test]
#[ignore = "requires a MySQL service and MYSQL_URL with CREATE DATABASE"]
async fn mysql_scope_contract() -> Result<(), OrmError> {
    use siderite_backends::mysql::MySqlBackend;
    use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};

    let url = required_url("MYSQL_URL")?;
    let admin = MySqlPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .map_err(driver_error)?;
    let name = format!("siderite_scopes_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .map_err(driver_error)?;
    let result = async {
        #[cfg(feature = "mysql-native")]
        if std::env::var_os("SIDERITE_TEST_NATIVE_MYSQL").is_some() {
            use siderite_backends::mysql::native::{NativeMySqlBackend, NativeMySqlOptions};
            let (server, _) = url
                .rsplit_once('/')
                .ok_or_else(|| QueryError::InvalidPlan("missing database in URL".into()))?;
            let options = NativeMySqlOptions {
                max_connections: 1,
                ..NativeMySqlOptions::default()
            };
            let backend =
                NativeMySqlBackend::connect_with(&format!("{server}/{name}"), options).await?;
            let result = exercise_live(Db::new(backend.clone())).await;
            let closed = backend.close().await;
            result?;
            closed?;
            return Ok(());
        }
        let options = url.parse::<MySqlConnectOptions>().map_err(driver_error)?;
        let pool = MySqlPoolOptions::new()
            .max_connections(1)
            .connect_with(options.database(&name))
            .await
            .map_err(driver_error)?;
        let db = Db::new(MySqlBackend::from_pool(pool.clone()));
        let result = exercise_live(db).await;
        pool.close().await;
        result
    }
    .await;
    let cleanup = sqlx::query(&format!("DROP DATABASE {name}"))
        .execute(&admin)
        .await
        .map_err(driver_error);
    admin.close().await;
    result?;
    cleanup?;
    Ok(())
}
