//! QuerySet builders and read terminals on SQLite.
#![cfg(feature = "sqlite")]
#![allow(clippy::unwrap_used)]

mod common;

use common::{Author, Book, Tag, db, seed, titles};
use siderite_orm::{Expr, Model, OrmError, QueryError};

#[tokio::test]
async fn filter_exclude_order_limit_offset() {
    let db = db().await;
    seed(&db).await;
    let books = Book::objects(&db)
        .filter(Book::likes.ge(3_i64))
        .exclude(Book::title.eq("Zig"))
        .order_by([Book::likes.desc()])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&books), ["Rust", "SQL", "Async"]);

    let page = Book::objects(&db)
        .order_by([Book::id.asc()])
        .offset(1)
        .limit(2)
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&page), ["Async", "SQL"]);
    let tail = Book::objects(&db)
        .order_by([Book::id.asc()])
        .offset(3)
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&tail), ["Go", "Zig"]);
}

#[tokio::test]
async fn first_last_earliest_latest_and_reverse() {
    let db = db().await;
    seed(&db).await;
    let books = Book::objects(&db);
    assert_eq!(books.clone().first().await.unwrap().unwrap().title, "Rust");
    assert_eq!(books.clone().last().await.unwrap().unwrap().title, "Zig");
    let by_name = Author::objects(&db);
    assert_eq!(by_name.clone().first().await.unwrap().unwrap().name, "Ann");
    assert_eq!(by_name.last().await.unwrap().unwrap().name, "Dee");
    assert_eq!(
        books.clone().earliest(Book::published).await.unwrap().title,
        "SQL"
    );
    assert_eq!(
        books.clone().latest(Book::published).await.unwrap().title,
        "Go"
    );
    let reversed = books
        .clone()
        .order_by([Book::id.asc()])
        .reverse()
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&reversed), ["Zig", "Go", "SQL", "Async", "Rust"]);
    let none = books.filter(Book::likes.gt(99_i64));
    assert!(matches!(
        none.clone().latest(Book::published).await,
        Err(OrmError::Query(QueryError::DoesNotExist))
    ));
    assert!(none.first().await.unwrap().is_none());
}

#[tokio::test]
async fn count_exists_contains_ignore_default_ordering_and_wrap_limits() {
    let db = db().await;
    let s = seed(&db).await;
    let authors = Author::objects(&db);
    assert_eq!(authors.clone().count().await.unwrap(), 4);
    assert_eq!(authors.clone().limit(3).count().await.unwrap(), 3);
    assert_eq!(authors.clone().offset(3).count().await.unwrap(), 1);
    assert_eq!(authors.clone().distinct().count().await.unwrap(), 4);
    let ages = authors.clone().project([Author::age.select()]).distinct();
    assert_eq!(
        ages.count().await.unwrap(),
        4,
        "NULL counts as a distinct value"
    );
    assert!(
        authors
            .clone()
            .filter(Author::age.gt(40))
            .exists()
            .await
            .unwrap()
    );
    assert!(
        !authors
            .clone()
            .filter(Author::age.gt(50))
            .exists()
            .await
            .unwrap()
    );
    assert!(authors.clone().limit(1).exists().await.unwrap());
    assert!(authors.clone().contains(&s.ann).await.unwrap());
    assert!(
        !authors
            .filter(Author::age.gt(40))
            .contains(&s.ann)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn none_never_touches_the_database() {
    let db = db().await;
    seed(&db).await;
    let empty = Book::objects(&db).none();
    assert!(empty.clone().all().await.unwrap().is_empty());
    assert_eq!(empty.clone().count().await.unwrap(), 0);
    assert!(!empty.clone().exists().await.unwrap());
    assert_eq!(empty.clone().delete().await.unwrap(), 0);
    assert_eq!(empty.update([Book::likes.set(1_i64)]).await.unwrap(), 0);
    assert_eq!(Book::objects(&db).count().await.unwrap(), 5);
}

#[tokio::test]
async fn get_reports_missing_and_multiple() {
    let db = db().await;
    let s = seed(&db).await;
    let found = Book::objects(&db).get(Book::title.eq("SQL")).await.unwrap();
    assert_eq!(found.author.id(), &s.bob.id);
    assert!(matches!(
        Book::objects(&db).get(Book::title.eq("nope")).await,
        Err(OrmError::Query(QueryError::DoesNotExist))
    ));
    assert!(matches!(
        Book::objects(&db).get(Book::likes.gt(0_i64)).await,
        Err(OrmError::Query(QueryError::MultipleObjectsReturned(2)))
    ));
    // Authors are ordered by name by default; `get` runs without it.
    let authors = Author::objects(&db);
    assert_eq!(
        authors.clone().get(Author::id.eq(s.ann.id)).await.unwrap(),
        s.ann
    );
    assert!(matches!(
        authors.get(Author::age.gt(20)).await,
        Err(OrmError::Query(QueryError::MultipleObjectsReturned(2)))
    ));
}

#[tokio::test]
async fn paginate_returns_items_and_totals() {
    let db = db().await;
    seed(&db).await;
    let qs = Book::objects(&db).order_by([Book::id.asc()]);
    let page = qs.clone().paginate(2, 2).await.unwrap();
    assert_eq!(
        (page.total, page.page, page.per_page, page.total_pages()),
        (5, 2, 2, 3)
    );
    assert_eq!(titles(&page.items), ["SQL", "Go"]);
    let last = qs.clone().paginate(3, 2).await.unwrap();
    assert_eq!(titles(&last.items), ["Zig"]);
    assert!(qs.clone().paginate(9, 2).await.unwrap().items.is_empty());
    assert!(qs.paginate(0, 2).await.is_err());
}

#[tokio::test]
async fn in_bulk_keys_by_primary_key() {
    let db = db().await;
    let s = seed(&db).await;
    let map = Book::objects(&db)
        .in_bulk([s.book("Go").id, s.book("SQL").id, 999])
        .await
        .unwrap();
    assert_eq!(map.len(), 2);
    assert_eq!(map[&s.book("Go").id].title, "Go");
    assert!(
        Book::objects(&db)
            .in_bulk(Vec::new())
            .await
            .unwrap()
            .is_empty()
    );
    let tags = Tag::objects(&db).in_bulk(["x".to_owned()]).await.unwrap();
    assert!(tags.is_empty());
}

#[tokio::test]
async fn values_and_values_list_decode_by_position() {
    let db = db().await;
    seed(&db).await;
    let rows = Book::objects(&db)
        .filter(Book::likes.ge(8_i64))
        .order_by([Book::likes.asc()])
        .values([Book::title.select(), Book::likes.select()])
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<String>("title").unwrap(), "SQL");
    let pairs: Vec<(String, i64)> = Book::objects(&db)
        .order_by([Book::likes.desc()])
        .limit(2)
        .values_list([Book::title.select(), Book::likes.select()])
        .await
        .unwrap();
    assert_eq!(pairs, [("Rust".to_owned(), 10), ("SQL".to_owned(), 8)]);
    let ages: Vec<Option<i32>> = Author::objects(&db).values_list(["age"]).await.unwrap();
    assert_eq!(ages, [Some(30), Some(25), None, Some(41)]);
    let bad = Book::objects(&db).values_list::<i64, _>(["title"]).await;
    assert!(matches!(
        bad,
        Err(OrmError::Query(QueryError::Decode { .. }))
    ));
}

#[tokio::test]
async fn update_sets_values_and_expressions() {
    let db = db().await;
    let s = seed(&db).await;
    let changed = Book::objects(&db)
        .filter(Book::author.eq(&s.ann))
        .update([
            Book::likes.set_expr(Book::likes + 100_i64),
            Book::pages.set(None),
            Book::title.set("Renamed"),
        ])
        .await
        .unwrap();
    assert_eq!(changed, 2);
    let ann_books = s
        .ann
        .books(&db)
        .order_by([Book::id.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(
        ann_books.iter().map(|b| b.likes).collect::<Vec<_>>(),
        [110, 105]
    );
    assert!(
        ann_books
            .iter()
            .all(|b| b.pages.is_none() && b.title == "Renamed")
    );
}

#[tokio::test]
async fn update_and_delete_with_limit_or_joins_use_key_subqueries() {
    let db = db().await;
    seed(&db).await;
    let limited = Book::objects(&db)
        .order_by([Book::likes.desc()])
        .limit(2)
        .update([Book::dislikes.set(0_i64)])
        .await
        .unwrap();
    assert_eq!(limited, 2);
    let zeroed = Book::objects(&db)
        .filter(Book::dislikes.eq(0_i64))
        .count()
        .await
        .unwrap();
    assert_eq!(zeroed, 3, "Go already had 0 dislikes");

    let red = Book::objects(&db)
        .filter(Book::author.join(Author::team).eq(1_i64))
        .delete()
        .await
        .unwrap();
    assert_eq!(red, 4);
    assert_eq!(Book::objects(&db).count().await.unwrap(), 1);

    let removed = Author::objects(&db)
        .order_by([Author::id.asc()])
        .limit(1)
        .delete()
        .await
        .unwrap();
    assert_eq!(removed, 1);
}

#[tokio::test]
async fn create_and_get_or_create() {
    let db = db().await;
    let s = seed(&db).await;
    let created = Book::objects(&db)
        .create(Book::new("New", s.bob.id))
        .await
        .unwrap();
    assert!(created.id > 0);

    let (again, was_created) = Tag::objects(&db)
        .get_or_create(Tag::slug.eq("rust"), || Tag::new("rust", "Rust"))
        .await
        .unwrap();
    assert!(was_created);
    let (same, was_created) = Tag::objects(&db)
        .get_or_create(Tag::slug.eq("rust"), || Tag::new("rust", "ignored"))
        .await
        .unwrap();
    assert!(!was_created);
    assert_eq!(same, again);
    assert_eq!(same.label, "Rust");
}

#[tokio::test]
async fn get_or_create_recovers_from_a_lost_race() {
    let db = db().await;
    Tag::objects(&db)
        .create(Tag::new("t", "first"))
        .await
        .unwrap();
    // The predicate misses the existing row, so the insert hits the unique key;
    // the follow-up lookup also misses, and the constraint error surfaces
    // carrying the lookup's error as context.
    let outcome = Tag::objects(&db)
        .get_or_create(Tag::label.eq("other"), || Tag::new("t", "other"))
        .await;
    let Err(OrmError::Backend(siderite_orm::BackendError::Constraint(message))) = outcome else {
        panic!("expected a constraint error, got {outcome:?}");
    };
    assert!(
        message.contains("re-fetch after the conflict failed"),
        "{message}"
    );
}

#[tokio::test]
async fn update_or_create_updates_or_inserts() {
    let db = db().await;
    let (tag, created) = Tag::objects(&db)
        .update_or_create(
            Tag::slug.eq("a"),
            || Tag::new("a", "one"),
            |t| t.label = "changed".into(),
        )
        .await
        .unwrap();
    assert!(created && tag.label == "one");
    let (tag, created) = Tag::objects(&db)
        .update_or_create(
            Tag::slug.eq("a"),
            || Tag::new("a", "unused"),
            |t| t.label = "changed".into(),
        )
        .await
        .unwrap();
    assert!(!created && tag.label == "changed");
    assert_eq!(Tag::objects(&db).count().await.unwrap(), 1);
}

#[tokio::test]
async fn update_or_create_reports_a_conflict_its_retry_cannot_resolve() {
    let db = db().await;
    Tag::objects(&db)
        .create(Tag::new("t", "first"))
        .await
        .unwrap();
    // The insert hits the existing key; the retry's lookup still misses, so
    // the constraint error surfaces with the lookup's error attached.
    let outcome = Tag::objects(&db)
        .update_or_create(
            Tag::label.eq("other"),
            || Tag::new("t", "other"),
            |t| t.label = "changed".into(),
        )
        .await;
    let Err(OrmError::Backend(siderite_orm::BackendError::Constraint(message))) = outcome else {
        panic!("expected a constraint error, got {outcome:?}");
    };
    assert!(
        message.contains("re-fetch after the conflict failed"),
        "{message}"
    );
    assert_eq!(
        Tag::objects(&db)
            .get(Tag::slug.eq("t"))
            .await
            .unwrap()
            .label,
        "first"
    );
}

#[tokio::test]
async fn using_switches_to_a_transaction_handle() {
    let db = db().await;
    seed(&db).await;
    let base = Book::objects(&db);
    let result: Result<(), OrmError> = db
        .transaction(|tx| async move {
            let removed = base.using(&tx).delete().await?;
            assert_eq!(removed, 5);
            Err(QueryError::InvalidPlan("rollback".into()).into())
        })
        .await;
    assert!(result.is_err());
    assert_eq!(Book::objects(&db).count().await.unwrap(), 5);
}

#[tokio::test]
async fn annotation_and_alias_names_are_validated() {
    let db = db().await;
    let clash = Book::objects(&db)
        .annotate("title", Expr::val(1))
        .all()
        .await;
    assert!(matches!(
        clash,
        Err(OrmError::Query(QueryError::InvalidPlan(_)))
    ));
}

/// Handlers run on a multi-threaded runtime: every terminal future must be `Send`.
#[tokio::test]
async fn terminal_futures_are_send() {
    fn is_send<T: Send>(_: T) {}
    let db = db().await;
    let book = Book::new("x", 1);
    is_send(Book::objects(&db).all());
    is_send(Book::objects(&db).get(Book::id.eq(1_i64)));
    is_send(Book::objects(&db).first());
    is_send(Book::objects(&db).count());
    is_send(Book::objects(&db).exists());
    is_send(Book::objects(&db).paginate(1, 2));
    is_send(Book::objects(&db).in_bulk([1_i64]));
    is_send(Book::objects(&db).values([Book::title.select()]));
    is_send(Book::objects(&db).aggregate([("n", siderite_orm::Count::all())]));
    is_send(Book::objects(&db).update([Book::likes.set(1_i64)]));
    is_send(Book::objects(&db).delete());
    is_send(Book::objects(&db).bulk_create(vec![book.clone()]));
    is_send(Book::objects(&db).bulk_update(&[], &["likes"]));
    is_send(Book::objects(&db).get_or_create(Book::id.eq(1_i64), || Book::new("x", 1)));
    is_send(Book::objects(&db).update_or_create(Book::id.eq(1_i64), || Book::new("x", 1), |_| {}));
    is_send(
        Book::objects(&db)
            .select_related(Book::author_relation())
            .all(),
    );
    is_send(
        Book::objects(&db)
            .prefetch_related(Book::author_relation())
            .all(),
    );
    is_send(book.tags(&db).add_pks([String::new()]));
    is_send(book.tags(&db).set_pks([String::new()]));
    let mut owned = book.clone();
    is_send(siderite_orm::ModelOps::save(&mut owned, &db));
    is_send(siderite_orm::ModelOps::delete(&book, &db));
}

#[tokio::test]
async fn correlated_subqueries_work_in_delete_and_update() {
    let db = db().await;
    let s = seed(&db).await;
    let has_books = || {
        Book::objects(&db)
            .filter(Book::author.expr().eq(Author::id.outer_ref()))
            .exists_expr()
    };
    let removed = Author::objects(&db)
        .exclude(has_books())
        .delete()
        .await
        .unwrap();
    assert_eq!(removed, 1, "only Cy has no books");
    assert!(!Author::objects(&db).contains(&s.cy).await.unwrap());

    let per_author = Book::objects(&db)
        .filter(Book::author.expr().eq(Author::id.outer_ref()))
        .subquery(("n", siderite_orm::Count::all()));
    let changed = Author::objects(&db)
        .update([Author::age.set_expr(Expr::subquery(per_author))])
        .await
        .unwrap();
    assert_eq!(changed, 3);
    let ages: Vec<_> = Author::objects(&db)
        .values_list::<Option<i32>, _>(["age"])
        .await
        .unwrap();
    assert_eq!(ages, [Some(2), Some(1), Some(2)]);
}
