use crate::common::{Author, Book};
use crate::fixtures::{TestDb, is_capability};
use siderite_backends::mongodb::MongoBackend;
use siderite_orm::Model as _;
use siderite_orm::{BackendError, Db, Feature, IsolationLevel, OrmError, RowNumber};

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn unsupported_features_fail_before_any_io() {
    // Nothing listens here: any I/O would fail with a connection error.
    let client = mongodb::Client::with_uri_str(
        "mongodb://127.0.0.1:1/?serverSelectionTimeoutMS=100&directConnection=true",
    )
    .await
    .unwrap();
    let db = Db::new(MongoBackend::from_client(client, "nowhere"));
    let books = || Book::objects(&db);

    let r = books().select_for_update().all().await;
    assert!(is_capability(&r, Feature::RowLocking), "{r:?}");
    let r = books().skip_locked().all().await;
    assert!(
        is_capability(&r, Feature::RowLocking) || is_capability(&r, Feature::LockModifiers),
        "{r:?}"
    );
    let r = books()
        .filter(Book::author.join(Author::name).eq("Ann"))
        .all()
        .await;
    assert!(is_capability(&r, Feature::Joins), "{r:?}");
    let r = books().select_related(Book::author_relation()).all().await;
    assert!(is_capability(&r, Feature::Joins), "{r:?}");
    let r = books().annotate("rn", RowNumber::new()).all().await;
    assert!(is_capability(&r, Feature::WindowFunctions), "{r:?}");
    let r = books().distinct_on([Book::author.expr()]).all().await;
    assert!(is_capability(&r, Feature::DistinctOn), "{r:?}");
    let r = books()
        .filter(Book::author.in_subquery(Author::objects(&db).subquery(Author::id.select())))
        .all()
        .await;
    assert!(is_capability(&r, Feature::Subqueries), "{r:?}");
    let r = books()
        .filter(Author::objects(&db).exists_expr())
        .count()
        .await;
    assert!(is_capability(&r, Feature::Subqueries), "{r:?}");
    let r = books().union(books()).unwrap().all().await;
    assert!(is_capability(&r, Feature::SetOperations), "{r:?}");
    let r = books()
        .annotate("arr", siderite_orm::ArrayAgg::of(Book::title))
        .all()
        .await;
    assert!(r.is_err());
    let r = books()
        .aggregate([("a", siderite_orm::ArrayAgg::of(Book::title))])
        .await;
    assert!(is_capability(&r, Feature::Arrays), "{r:?}");
    for r in [
        db.raw_sql("SELECT 1", vec![]).await.map(|_| ()),
        db.raw_execute("DELETE FROM x", vec![]).await.map(|_| ()),
        db.execute_script("CREATE TABLE x (id INT)").await,
    ] {
        assert!(is_capability(&r, Feature::RawSql), "{r:?}");
    }
    // Isolation levels are rejected before a session starts.
    let r: Result<(), OrmError> = db
        .transaction_with(IsolationLevel::ReadCommitted, |_| async { Ok(()) })
        .await;
    assert!(
        is_capability(&r, Feature::Isolation(IsolationLevel::ReadCommitted)),
        "{r:?}"
    );
    // Whereas a supported plan does reach the network.
    let r = books().count().await;
    assert!(
        matches!(r, Err(OrmError::Backend(BackendError::Connection(_)))),
        "{r:?}"
    );
}

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn raw_commands_reach_the_server() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let reply = t
        .backend
        .raw_command(mongodb::bson::doc! { "ping": 1 })
        .await
        .unwrap();
    assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    t.cleanup().await;
}
