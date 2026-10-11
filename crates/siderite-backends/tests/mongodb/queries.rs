use crate::common::{Author, Book, titles};
use crate::fixtures::TestDb;
use siderite_orm::Model as _;
use siderite_orm::{Avg, Count, Expr, Max, Min, StdDev, StringAgg, Sum, Variance};

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn ordering_paging_distinct_count_exists() {
    let Some((t, _)) = TestDb::seeded().await else {
        return;
    };
    let db = &t.db;
    let page = Book::objects(db)
        .order_by([Book::likes.desc()])
        .offset(1)
        .limit(2)
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&page), ["SQL", "Async"]);
    assert_eq!(Book::objects(db).count().await.unwrap(), 5);
    assert_eq!(Book::objects(db).limit(2).count().await.unwrap(), 2);
    assert_eq!(
        Book::objects(db)
            .filter(Book::likes.gt(100))
            .count()
            .await
            .unwrap(),
        0
    );
    assert!(Book::objects(db).exists().await.unwrap());
    assert!(
        !Book::objects(db)
            .filter(Book::likes.gt(100))
            .exists()
            .await
            .unwrap()
    );
    assert!(Book::objects(db).none().all().await.unwrap().is_empty());
    let authors: Vec<i64> = {
        let mut v = Book::objects(db)
            .distinct()
            .values_list(["author_id"])
            .await
            .unwrap();
        v.sort_unstable();
        v
    };
    assert_eq!(authors.len(), 3);
    assert_eq!(
        Book::objects(db)
            .distinct()
            .values_list::<i64, _>(["author_id"])
            .await
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        Book::objects(db)
            .project(["author_id"])
            .distinct()
            .count()
            .await
            .unwrap(),
        3
    );
    let first = Book::objects(db)
        .order_by([Book::pages.asc()])
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.title, "Async", "NULLs sort first ascending");
    let last = Book::objects(db)
        .order_by([Book::likes.asc()])
        .last()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(last.title, "Rust");
    let p = Book::objects(db)
        .order_by([Book::title.asc()])
        .paginate(2, 2)
        .await
        .unwrap();
    assert_eq!((p.total, titles(&p.items)), (5, vec!["Rust", "SQL"]));
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn aggregates_grouping_and_having() {
    let Some((t, s)) = TestDb::seeded().await else {
        return;
    };
    let db = &t.db;
    let row = Book::objects(db)
        .aggregate([
            ("n", Count::all()),
            ("with_pages", Count::of(Book::pages)),
            ("likes", Sum::of(Book::likes)),
            ("mean", Avg::of(Book::likes)),
            ("lo", Min::of(Book::likes)),
            ("hi", Max::of(Book::likes)),
            ("sd", StdDev::sample(Book::likes)),
            ("sdp", StdDev::population(Book::likes)),
            ("var", Variance::sample(Book::likes)),
            ("authors", Count::of(Book::author).distinct()),
            ("big", Sum::of(Book::likes).filter(Book::likes.gt(4))),
            ("titles", Count::all().filter(Book::title.starts_with("S"))),
        ])
        .await
        .unwrap();
    assert_eq!(row.get_as::<i64>("n").unwrap(), 5);
    assert_eq!(row.get_as::<i64>("with_pages").unwrap(), 4);
    assert_eq!(row.get_as::<i64>("likes").unwrap(), 27);
    assert!((row.get_as::<f64>("mean").unwrap() - 5.4).abs() < 1e-9);
    assert_eq!(
        (
            row.get_as::<i64>("lo").unwrap(),
            row.get_as::<i64>("hi").unwrap()
        ),
        (1, 10)
    );
    let var = row.get_as::<f64>("var").unwrap();
    assert!((row.get_as::<f64>("sd").unwrap() - var.sqrt()).abs() < 1e-9);
    assert!((var - 13.3).abs() < 1e-9, "{var}");
    assert!((row.get_as::<f64>("sdp").unwrap().powi(2) - 10.64).abs() < 1e-9);
    assert_eq!(row.get_as::<i64>("authors").unwrap(), 3);
    assert_eq!(row.get_as::<i64>("big").unwrap(), 23);
    assert_eq!(row.get_as::<i64>("titles").unwrap(), 1);

    // Aggregates over an empty queryset still produce one row.
    let empty = Book::objects(db)
        .filter(Book::likes.gt(1000))
        .aggregate([
            ("n", Count::all()),
            ("total", Sum::of(Book::likes)),
            ("avg", Avg::of(Book::likes)),
        ])
        .await
        .unwrap();
    assert_eq!(empty.get_as::<i64>("n").unwrap(), 0);
    assert_eq!(empty.get_as::<Option<i64>>("total").unwrap(), None);
    assert_eq!(empty.get_as::<Option<f64>>("avg").unwrap(), None);
    let none = Book::objects(db)
        .none()
        .aggregate([("n", Count::all())])
        .await
        .unwrap();
    assert_eq!(none.get_as::<i64>("n").unwrap(), 0);
    // SUM of only-NULL values is NULL, not 0.
    let nulls = Author::objects(db)
        .filter(Author::age.is_null())
        .aggregate([("total", Sum::of(Author::age))])
        .await
        .unwrap();
    assert_eq!(nulls.get_as::<Option<i64>>("total").unwrap(), None);

    // Aggregate over a limited (derived) queryset.
    let limited = Book::objects(db)
        .order_by([Book::likes.desc()])
        .limit(2)
        .aggregate([("total", Sum::of(Book::likes))])
        .await
        .unwrap();
    assert_eq!(limited.get_as::<i64>("total").unwrap(), 18);

    // GROUP BY with HAVING and ordering by an aggregate.
    let grouped = Book::objects(db)
        .project([Book::author.select()])
        .annotate("n", Count::all())
        .annotate("total", Sum::of(Book::likes))
        .filter(Expr::from(Count::all()).gt(1_i64))
        .order_by([Expr::col("total").desc()])
        .rows()
        .await
        .unwrap();
    let got: Vec<(i64, i64, i64)> = grouped
        .iter()
        .map(|r| {
            (
                r.get_as("author_id").unwrap(),
                r.get_as("n").unwrap(),
                r.get_as("total").unwrap(),
            )
        })
        .collect();
    assert_eq!(got, [(s.ann.id, 2, 15), (s.dee.id, 2, 4)]);

    let joined = Book::objects(db)
        .filter(Book::author.eq(s.ann.id))
        .aggregate([("titles", StringAgg::of(Book::title, ", "))])
        .await
        .unwrap();
    let mut parts: Vec<String> = joined
        .get_as::<String>("titles")
        .unwrap()
        .split(", ")
        .map(str::to_owned)
        .collect();
    parts.sort();
    assert_eq!(parts, ["Async", "Rust"]);
    t.cleanup().await;
}
