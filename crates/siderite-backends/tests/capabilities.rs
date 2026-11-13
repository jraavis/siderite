//! Capability gating: unsupported features fail before any I/O.
#![cfg(feature = "sqlite")]
#![allow(clippy::unwrap_used)]

mod common;

use common::{Book, db, seed};
use siderite_orm::{BackendCapabilityError, BackendKind, Feature, Model, OrmError, StdDev, Sum};

fn is_unsupported(result: &Result<impl std::fmt::Debug, OrmError>, wanted: Feature) -> bool {
    matches!(
        result,
        Err(OrmError::Capability(BackendCapabilityError::Unsupported {
            backend: BackendKind::Sqlite,
            feature,
        })) if *feature == wanted
    )
}

#[tokio::test]
async fn row_locking_is_rejected_by_sqlite_before_io() {
    let db = db().await;
    seed(&db).await;
    for qs in [
        Book::objects(&db).select_for_update(),
        Book::objects(&db).select_for_update().nowait(),
        Book::objects(&db).skip_locked(),
    ] {
        assert!(matches!(
            qs.clone().all().await,
            Err(OrmError::Capability(
                BackendCapabilityError::RowLockingUnsupported { .. }
            ))
        ));
        assert!(qs.clone().paginate(1, 2).await.is_err());
    }
}

#[tokio::test]
async fn paginate_checks_the_plan_before_counting() {
    // No schema: a query that ran would fail with "no such table", so the
    // capability error proves nothing was executed.
    let bare = siderite_orm::Db::new(
        common::TestSqlite::connect("sqlite::memory:")
            .await
            .unwrap(),
    );
    let outcome = Book::objects(&bare)
        .select_for_update()
        .paginate(1, 5)
        .await;
    assert!(matches!(outcome, Err(OrmError::Capability(_))));
}

#[tokio::test]
async fn statistical_aggregates_and_regex_and_distinct_on_are_gated() {
    let db = db().await;
    seed(&db).await;
    let deviation = Book::objects(&db)
        .aggregate([("sd", StdDev::population(Book::likes))])
        .await;
    assert!(is_unsupported(&deviation, Feature::StatisticalAggregates));
    let regex = Book::objects(&db)
        .filter(Book::title.regex("^R"))
        .all()
        .await;
    assert!(is_unsupported(&regex, Feature::Regex));
    let distinct_on = Book::objects(&db).distinct_on([Book::author]).all().await;
    assert!(is_unsupported(&distinct_on, Feature::DistinctOn));
    let array = Book::objects(&db)
        .aggregate([("all", siderite_orm::ArrayAgg::of(Book::title))])
        .await;
    assert!(is_unsupported(&array, Feature::Arrays));
    // Supported aggregates still work.
    let total = Book::objects(&db)
        .aggregate([("n", Sum::of(Book::likes))])
        .await
        .unwrap();
    assert_eq!(total.get_as::<i64>("n").unwrap(), 27);
}
