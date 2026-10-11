use crate::common::{Author, Book, Tag};
use crate::fixtures::{TestDb, is_capability};
use siderite_orm::{BackendError, Feature, OrmError};
use siderite_orm::{Model as _, ModelOps as _};

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn updates_deletes_and_f_expressions() {
    let Some((t, _)) = TestDb::seeded().await else {
        return;
    };
    let db = &t.db;
    let n = Book::objects(db)
        .filter(Book::likes.lt(6))
        .update([
            Book::likes.set_expr(Book::likes + 10_i64),
            Book::pages.set(7),
        ])
        .await
        .unwrap();
    assert_eq!(n, 3);
    let mut likes: Vec<i64> = Book::objects(db)
        .all()
        .await
        .unwrap()
        .iter()
        .map(|b| b.likes)
        .collect();
    likes.sort_unstable();
    assert_eq!(likes, [8, 10, 11, 13, 15]);
    // Literal-only update, NULL assignment, no match.
    let n = Author::objects(db)
        .filter(Author::name.eq("Ann"))
        .update([Author::age.set(None::<i32>)])
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        Author::objects(db)
            .filter(Author::age.is_null())
            .count()
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        Author::objects(db)
            .filter(Author::name.eq("nobody"))
            .update([Author::age.set(1)])
            .await
            .unwrap(),
        0
    );
    // A limited queryset needs `pk IN (subquery)`, which MongoDB lacks.
    let r = Book::objects(db).limit(1).delete().await;
    assert!(is_capability(&r, Feature::Subqueries), "{r:?}");
    assert_eq!(
        Book::objects(db)
            .filter(Book::likes.gt(12))
            .delete()
            .await
            .unwrap(),
        2
    );
    assert_eq!(Book::objects(db).count().await.unwrap(), 3);
    // MongoDB has no foreign keys: deleting an author keeps their books.
    Author::objects(db)
        .filter(Author::name.eq("Dee"))
        .delete()
        .await
        .unwrap();
    assert_eq!(Author::objects(db).count().await.unwrap(), 3);
    assert_eq!(Book::objects(db).count().await.unwrap(), 3);
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn create_family_and_bulk_operations() {
    let Some((t, s)) = TestDb::seeded().await else {
        return;
    };
    let db = &t.db;
    let (found, created) = Author::objects(db)
        .get_or_create(Author::name.eq("Ann"), || Author::new("Ann", None))
        .await
        .unwrap();
    assert!(!created && found.id == s.ann.id);
    let (fresh, created) = Author::objects(db)
        .get_or_create(Author::name.eq("Eve"), || Author::new("Eve", Some(9)))
        .await
        .unwrap();
    assert!(created && fresh.id > s.dee.id);
    let (updated, created) = Author::objects(db)
        .update_or_create(
            Author::name.eq("Eve"),
            || Author::new("Eve", None),
            |a| a.age = Some(10),
        )
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(updated.age, Some(10));
    assert_eq!(
        Author::objects(db)
            .get(Author::name.eq("Eve"))
            .await
            .unwrap()
            .age,
        Some(10)
    );
    let (made, created) = Author::objects(db)
        .update_or_create(
            Author::name.eq("Fay"),
            || Author::new("Fay", Some(1)),
            |_| {},
        )
        .await
        .unwrap();
    assert!(created && made.id > fresh.id);

    let mut authors = Author::objects(db)
        .order_by([Author::id.asc()])
        .all()
        .await
        .unwrap();
    for a in &mut authors {
        a.age = Some(a.age.unwrap_or(0) + 1);
    }
    let changed = Author::objects(db)
        .bulk_update(&authors, &["age"])
        .await
        .unwrap();
    assert_eq!(changed, authors.len() as u64);
    assert_eq!(
        Author::objects(db)
            .get(Author::name.eq("Cy"))
            .await
            .unwrap()
            .age,
        Some(1)
    );

    let map = Author::objects(db)
        .in_bulk([s.ann.id, s.bob.id, 9999])
        .await
        .unwrap();
    assert_eq!(map.len(), 2);
    assert!(Author::objects(db).contains(&s.ann).await.unwrap());
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn duplicate_keys_and_unique_indexes_are_constraint_errors() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let tag = Tag {
        slug: "rust".into(),
        label: "Rust".into(),
    };
    Tag::objects(db).create(tag.clone()).await.unwrap();
    let err = Tag::objects(db).create(tag).await;
    assert!(
        matches!(err, Err(OrmError::Backend(BackendError::Constraint(_)))),
        "{err:?}"
    );
    assert_eq!(Tag::objects(db).count().await.unwrap(), 1);

    t.backend
        .create_unique_index("authors", "name")
        .await
        .unwrap();
    Author::new("uniq", None).save(db).await.unwrap();
    let err = Author::new("uniq", None).save(db).await;
    assert!(
        matches!(err, Err(OrmError::Backend(BackendError::Constraint(_)))),
        "{err:?}"
    );
    // A duplicate inside a bulk insert fails the whole transaction.
    let before = Author::objects(db).count().await.unwrap();
    let err = Author::objects(db)
        .bulk_create(vec![Author::new("x1", None), Author::new("uniq", None)])
        .await;
    assert!(
        matches!(err, Err(OrmError::Backend(BackendError::Constraint(_)))),
        "{err:?}"
    );
    assert_eq!(Author::objects(db).count().await.unwrap(), before);
    t.cleanup().await;
}
