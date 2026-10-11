use crate::common::Author;
use crate::fixtures::{TestDb, is_capability};
use siderite_orm::{Feature, IsolationLevel, OrmError};
use siderite_orm::{Model as _, ModelOps as _};

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn transactions_commit_and_roll_back() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    Author::new("outside", None).save(db).await.unwrap();
    db.transaction(|tx| async move {
        Author::new("committed", Some(1)).save(&tx).await?;
        // Reads inside the transaction see its own writes; others do not.
        assert_eq!(Author::objects(&tx).count().await?, 2);
        Ok::<_, OrmError>(())
    })
    .await
    .unwrap();
    assert_eq!(Author::objects(db).count().await.unwrap(), 2);

    let result: Result<(), OrmError> = db
        .transaction(|tx| async move {
            Author::new("rolled back", Some(2)).save(&tx).await?;
            Author::objects(&tx)
                .filter(Author::name.eq("outside"))
                .delete()
                .await?;
            assert_eq!(Author::objects(&tx).count().await?, 2);
            Err(siderite_orm::QueryError::InvalidPlan("boom".into()).into())
        })
        .await;
    assert!(result.is_err());
    assert_eq!(Author::objects(db).count().await.unwrap(), 2);
    assert!(
        Author::objects(db)
            .filter(Author::name.eq("outside"))
            .exists()
            .await
            .unwrap()
    );

    // Isolation is not requestable; nested transactions need savepoints.
    let r: Result<(), OrmError> = db
        .transaction_with(IsolationLevel::Serializable, |_| async { Ok(()) })
        .await;
    assert!(
        is_capability(&r, Feature::Isolation(IsolationLevel::Serializable)),
        "{r:?}"
    );
    let r: Result<(), OrmError> = db
        .transaction(|tx| async move { tx.transaction(|_| async { Ok::<_, OrmError>(()) }).await })
        .await;
    assert!(is_capability(&r, Feature::Savepoints), "{r:?}");
    // bulk_create inside a transaction is a nested transaction.
    assert_eq!(Author::objects(db).count().await.unwrap(), 2);
    t.cleanup().await;
}
