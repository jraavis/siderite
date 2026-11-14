//! The derived `Model::from_cells` decodes every header shape exactly as
//! `from_row_mut` does, and querysets decode the same models through it on
//! the SQLx and the native SQLite backend.
#![allow(clippy::unwrap_used)]

use siderite::orm::{Cells, ColumnPlan, QueryError, Relation, Row, Value};
use siderite::prelude::*;
use siderite_backends::sqlite::SqliteBackend;
use siderite_backends::sqlite::native::NativeSqliteBackend;

#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "authors", ordering = ["id"])]
struct Author {
    #[field(primary_key, auto)]
    id: i64,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "books", ordering = ["id"])]
struct Book {
    #[field(primary_key, auto)]
    id: i64,
    title: String,
    author: ForeignKey<Author>,
    #[field(column = "editor")]
    reviewer: Option<ForeignKey<Author>>,
    #[field(skip)]
    scratch: String,
}

fn row(columns: &[(&str, Value)]) -> Row {
    Row::new(
        columns
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect(),
    )
}

/// `from_cells` through a plan for `row`'s header, checked against
/// `from_row_mut`.
fn decode(row: &Row, prefix: &str) -> Result<Book, QueryError> {
    let plan = ColumnPlan::new(Book::META, row.columns(), prefix);
    let planned = Book::from_cells(&mut row.clone(), &plan);
    assert_eq!(planned, Book::from_row_mut(&mut row.clone(), prefix));
    planned
}

fn book(id: i64, title: &str, author: i64, reviewer: Option<i64>) -> Book {
    Book {
        id,
        title: title.into(),
        author: ForeignKey::new(author),
        reviewer: reviewer.map(ForeignKey::new),
        scratch: String::new(),
    }
}

#[test]
fn derived_decoding_follows_the_header() {
    let reordered = row(&[
        ("editor", Value::Null),
        ("author_id", Value::Int(2)),
        ("title", Value::Text("t".into())),
        ("n", Value::Int(9)),
        ("id", Value::Int(1)),
    ]);
    assert_eq!(decode(&reordered, ""), Ok(book(1, "t", 2, None)));

    let joined = row(&[
        ("id", Value::Int(1)),
        ("b__id", Value::Int(5)),
        ("b__title", Value::Text("j".into())),
        ("b__author_id", Value::Int(6)),
        ("b__editor", Value::Int(7)),
    ]);
    assert_eq!(decode(&joined, "b__"), Ok(book(5, "j", 6, Some(7))));

    let duplicated = row(&[
        ("title", Value::Text("first".into())),
        ("title", Value::Text("second".into())),
        ("id", Value::Int(1)),
        ("author_id", Value::Int(2)),
        ("editor", Value::Null),
    ]);
    assert_eq!(decode(&duplicated, ""), Ok(book(1, "second", 2, None)));
}

#[test]
fn derived_decoding_errors_name_the_column() {
    let missing = row(&[("id", Value::Int(1)), ("title", Value::Text("t".into()))]);
    assert_eq!(
        decode(&missing, "b__"),
        Err(QueryError::Decode {
            column: "b__id".into(),
            reason: "column missing from result".into(),
        })
    );
    assert_eq!(
        decode(&missing, ""),
        Err(QueryError::Decode {
            column: "author_id".into(),
            reason: "column missing from result".into(),
        })
    );
    let mistyped = row(&[
        ("id", Value::Int(1)),
        ("title", Value::Int(3)),
        ("author_id", Value::Int(2)),
        ("editor", Value::Null),
    ]);
    let Err(QueryError::Decode { column, .. }) = decode(&mistyped, "") else {
        panic!("expected a decode error");
    };
    assert_eq!(column, "title");
}

const SCHEMA: &str = "
    CREATE TABLE authors (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);
    CREATE TABLE books (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL,
        author_id INTEGER NOT NULL REFERENCES authors(id), editor INTEGER);
    INSERT INTO authors (name) VALUES ('Ann'), ('Bo');
    INSERT INTO books (title, author_id, editor) VALUES ('a', 1, NULL), ('b', 2, 1), ('c', 1, 2);";

async fn databases() -> [Db; 2] {
    let sqlx = Db::new(SqliteBackend::connect("sqlite::memory:").await.unwrap());
    let native = Db::new(
        NativeSqliteBackend::connect("sqlite::memory:")
            .await
            .unwrap(),
    );
    for db in [&sqlx, &native] {
        db.execute_script(SCHEMA).await.unwrap();
    }
    [sqlx, native]
}

#[tokio::test]
async fn querysets_decode_the_same_models_on_both_backends() {
    let expected = vec![
        book(1, "a", 1, None),
        book(2, "b", 2, Some(1)),
        book(3, "c", 1, Some(2)),
    ];
    for db in databases().await {
        assert_eq!(Book::objects(&db).all().await.unwrap(), expected);
        let got = Book::objects(&db).get(Book::id.eq(2)).await.unwrap();
        assert_eq!(got, expected[1]);
        let none = Book::objects(&db).filter(Book::id.eq(9)).all().await;
        assert_eq!(none.unwrap(), Vec::new());

        let annotated = Book::objects(&db)
            .annotate("twice", Book::id.expr() + Book::id.expr())
            .all_annotated()
            .await
            .unwrap();
        let twice: Vec<i64> = annotated
            .iter()
            .map(|(_, row)| row.get_as("twice").unwrap())
            .collect();
        assert_eq!(twice, [2, 4, 6]);
        let plain = Book::objects(&db)
            .annotate("twice", Book::id.expr() + Book::id.expr())
            .all()
            .await
            .unwrap();
        assert_eq!(plain, expected);

        let related = Book::objects(&db)
            .select_related(Relation::new(Book::author, |b| &mut b.author))
            .all()
            .await
            .unwrap();
        let names: Vec<_> = related
            .iter()
            .map(|b| b.author.cached().map(|a| a.name.clone()))
            .collect();
        assert_eq!(
            names,
            [Some("Ann".into()), Some("Bo".into()), Some("Ann".into())]
        );

        let in_tx = db
            .transaction(|tx| async move { Book::objects(&tx).all().await })
            .await;
        assert_eq!(in_tx.unwrap(), expected);
    }
}

#[tokio::test]
async fn decode_errors_match_on_both_backends() {
    for db in databases().await {
        db.execute_script("UPDATE books SET title = x'00' WHERE id = 2")
            .await
            .unwrap();
        let raw = db
            .raw_sql("SELECT id, title, author_id, editor FROM books", Vec::new())
            .await
            .unwrap();
        let expected = Book::from_row(&raw.rows[1], "").unwrap_err();
        assert!(matches!(&expected, QueryError::Decode { column, .. } if column == "title"));
        let err = Book::objects(&db).all().await.unwrap_err();
        assert_eq!(err.to_string(), OrmError::from(expected).to_string());
    }
}
