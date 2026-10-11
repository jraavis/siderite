use crate::common::{Author, Book, titles};
use crate::fixtures::TestDb;
use siderite_orm::functions::{coalesce, concat, length, lower, upper};
use siderite_orm::{Expr, Value};
use siderite_orm::{Model as _, ModelOps as _};

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn lookups_follow_sql_semantics() {
    let Some((t, _)) = TestDb::seeded().await else {
        return;
    };
    let db = &t.db;
    let names = |qs: siderite_orm::QuerySet<Author>| async move {
        let mut v: Vec<String> = qs
            .all()
            .await
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        names(Author::objects(db).filter(Author::name.icontains("A"))).await,
        ["Ann"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::name.contains("a"))).await,
        Vec::<String>::new()
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::name.starts_with("B"))).await,
        ["Bob"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::name.ends_with("y"))).await,
        ["Cy"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::name.iexact("dEe"))).await,
        ["Dee"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::name.regex("^[AB]"))).await,
        ["Ann", "Bob"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::name.is_in(["Ann", "Cy", "Zed"]))).await,
        ["Ann", "Cy"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::age.range(25, 30))).await,
        ["Ann", "Bob"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::age.is_null())).await,
        ["Cy"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::age.eq(None::<i32>))).await,
        ["Cy"]
    );
    // NULL never satisfies a comparison, negated or not: Cy has no age.
    assert_eq!(
        names(Author::objects(db).filter(Author::age.ne(30))).await,
        ["Bob", "Dee"]
    );
    assert_eq!(
        names(Author::objects(db).exclude(Author::age.eq(30))).await,
        ["Bob", "Dee"]
    );
    assert_eq!(
        names(Author::objects(db).exclude(Author::age.gt(26))).await,
        ["Bob"]
    );
    assert_eq!(
        names(Author::objects(db).exclude(Author::age.range(25, 30))).await,
        ["Dee"]
    );
    assert_eq!(
        names(Author::objects(db).exclude(Author::age.is_in([30, 25]))).await,
        ["Dee"]
    );
    assert_eq!(
        names(Author::objects(db).exclude(Author::name.icontains("n"))).await,
        ["Bob", "Cy", "Dee"]
    );
    assert_eq!(
        names(Author::objects(db).exclude(Author::age.gt(26).and(Author::name.starts_with("D"))))
            .await,
        ["Ann", "Bob", "Cy"],
        "NOT (NULL AND FALSE) is true in SQL"
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::age.lt(26).or(Author::age.gt(40)))).await,
        ["Bob", "Dee"]
    );
    // Special characters are literal in contains/startswith.
    let mut odd = Author::new("a.b*c[", None);
    odd.save(db).await.unwrap();
    assert_eq!(
        names(Author::objects(db).filter(Author::name.contains(".b*"))).await,
        ["a.b*c["]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::name.icontains("C["))).await,
        ["a.b*c["]
    );
    assert_eq!(
        names(
            Author::objects(db).filter(
                Author::name
                    .contains("a.b*c[")
                    .and(Author::name.ends_with("c["))
            )
        )
        .await,
        ["a.b*c["]
    );
    // Empty AND / OR.
    assert_eq!(
        Author::objects(db)
            .filter(Expr::And(vec![]))
            .count()
            .await
            .unwrap(),
        5
    );
    assert_eq!(
        Author::objects(db)
            .filter(Expr::Or(vec![]))
            .count()
            .await
            .unwrap(),
        0
    );
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn expressions_compile_to_aggregation_operators() {
    let Some((t, _)) = TestDb::seeded().await else {
        return;
    };
    let db = &t.db;
    // Column-to-column comparison and arithmetic.
    let hated = Book::objects(db)
        .filter(Book::likes.gt(Book::dislikes + 1_i64))
        .order_by([Book::title.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&hated), ["Rust", "SQL"]);
    // NOT over an $expr keeps NULL rows out: `pages` is NULL for Async.
    let not_short = Book::objects(db)
        .exclude(Book::pages.expr().lt(Book::likes.expr() * 20_i64))
        .order_by([Book::title.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&not_short), ["Go", "Rust", "Zig"]);
    let rows = Book::objects(db)
        .filter(Book::title.eq("Rust"))
        .project([Book::title.select(), ("t", Book::title.expr()).into()])
        .rows()
        .await
        .unwrap();
    assert_eq!(rows[0].get("t"), Some(&Value::Text("Rust".into())));
    let rows = Book::objects(db)
        .filter(Book::title.eq("Rust"))
        .annotate("shout", upper(Book::title.expr()))
        .annotate("size", length(Book::title.expr()))
        .annotate("pg", coalesce([Book::pages.expr(), Expr::val(0)]))
        .annotate(
            "both",
            concat([
                Book::title.expr(),
                Expr::val("-"),
                lower(Book::title.expr()),
            ]),
        )
        .annotate("net", Book::likes.expr() - Book::dislikes.expr())
        .project(["title"])
        .rows()
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let plain = Book::objects(db)
        .order_by([lower(Book::title.expr()).desc()])
        .limit(2)
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&plain), ["Zig", "SQL"]);
    // Date parts on stored dates.
    let y2022 = Book::objects(db)
        .filter(Book::published.year().eq(2022_i64))
        .order_by([Book::title.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&y2022), ["Go", "Zig"]);
    let feb = Book::objects(db)
        .filter(Book::published.month().eq(2_i64))
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&feb), ["Zig"]);
    t.cleanup().await;
}
