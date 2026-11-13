//! Hand-written `Model` implementations: the reference for what
//! `#[derive(Model)]` generates, and an end-to-end test of the ORM contract
//! (write plans, querysets, transactions) on SQLite.
#![cfg(feature = "sqlite")]
#![allow(clippy::unwrap_used, non_upper_case_globals, dead_code)]

#[path = "common/backend.rs"]
mod backend;

use backend::TestSqlite;
use siderite_orm::{
    Db, DbType, Expr, Feature, Field, FieldMeta, ForeignKey, InsertPlan, IsolationLevel, Model,
    ModelMeta, OnDelete, OrderDirection, OrmError, QueryError, RelationKind, RelationMeta, Row,
    SqlType, Value, WritePlan, read_column,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone, PartialEq)]
struct Author {
    id: i64,
    name: String,
}

// ---- what `#[derive(Model)] #[model(table = "authors", ordering = ["name"])]` generates ----

impl Author {
    const id: Field<Author, i64> = Field::new("id");
    const name: Field<Author, String> = Field::new("name");
}

static AUTHOR_META: ModelMeta = ModelMeta {
    name: "Author",
    table: "authors",
    fields: &[
        FieldMeta {
            primary_key: true,
            auto: true,
            ..FieldMeta::new("id", "id", <i64 as DbType>::SQL_TYPE)
        },
        FieldMeta {
            max_length: Some(100),
            ..FieldMeta::new("name", "name", <String as DbType>::SQL_TYPE)
        },
    ],
    many_to_many: &[],
    ordering: &[("name", OrderDirection::Asc)],
    indexes: &[],
    constraints: &[],
    managed: true,
};

impl Model for Author {
    type Pk = i64;
    const META: &'static ModelMeta = &AUTHOR_META;

    fn pk(&self) -> i64 {
        self.id
    }
    fn set_pk(&mut self, value: Value) -> Result<(), QueryError> {
        self.id = siderite_orm::types::decode("id", value)?;
        Ok(())
    }
    fn is_unsaved(&self) -> bool {
        self.id == i64::default()
    }
    fn to_values(&self) -> Vec<(&'static str, Value)> {
        vec![("id", self.id.to_value()), ("name", self.name.to_value())]
    }
    fn from_row(row: &Row, prefix: &str) -> Result<Self, QueryError> {
        Ok(Self {
            id: read_column(row, prefix, "id")?,
            name: read_column(row, prefix, "name")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Book {
    id: i64,
    title: String,
    author: ForeignKey<Author>,
    pages: Option<i32>,
}

impl Book {
    const id: Field<Book, i64> = Field::new("id");
    const title: Field<Book, String> = Field::new("title");
    const author: Field<Book, ForeignKey<Author>> = Field::new("author_id");
}

fn author_meta() -> &'static ModelMeta {
    Author::META
}

static BOOK_META: ModelMeta = ModelMeta {
    name: "Book",
    table: "books",
    fields: &[
        FieldMeta {
            primary_key: true,
            auto: true,
            ..FieldMeta::new("id", "id", <i64 as DbType>::SQL_TYPE)
        },
        FieldMeta::new("title", "title", <String as DbType>::SQL_TYPE),
        FieldMeta {
            index: true,
            relation: Some(RelationMeta {
                kind: RelationKind::ForeignKey,
                target: author_meta,
                on_delete: OnDelete::Cascade,
                related_name: Some("books"),
            }),
            ..FieldMeta::new(
                "author",
                "author_id",
                <ForeignKey<Author> as DbType>::SQL_TYPE,
            )
        },
        FieldMeta {
            nullable: <Option<i32> as DbType>::NULLABLE,
            ..FieldMeta::new("pages", "pages", <Option<i32> as DbType>::SQL_TYPE)
        },
    ],
    many_to_many: &[],
    ordering: &[],
    indexes: &[],
    constraints: &[],
    managed: true,
};

impl Model for Book {
    type Pk = i64;
    const META: &'static ModelMeta = &BOOK_META;

    fn pk(&self) -> i64 {
        self.id
    }
    fn set_pk(&mut self, value: Value) -> Result<(), QueryError> {
        self.id = siderite_orm::types::decode("id", value)?;
        Ok(())
    }
    fn is_unsaved(&self) -> bool {
        self.id == i64::default()
    }
    fn to_values(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("id", self.id.to_value()),
            ("title", self.title.to_value()),
            ("author_id", self.author.to_value()),
            ("pages", self.pages.to_value()),
        ]
    }
    fn from_row(row: &Row, prefix: &str) -> Result<Self, QueryError> {
        Ok(Self {
            id: read_column(row, prefix, "id")?,
            title: read_column(row, prefix, "title")?,
            author: read_column(row, prefix, "author_id")?,
            pages: read_column(row, prefix, "pages")?,
        })
    }
}

/// Reverse accessor generated from `related_name = "books"`.
impl Author {
    fn books(&self, db: &Db) -> siderite_orm::QuerySet<Book> {
        Book::objects(db).filter(Book::author.expr().eq(self.id))
    }
}

// ---- tests ----

async fn db() -> Db {
    let db = Db::new(TestSqlite::connect("sqlite::memory:").await.unwrap());
    db.execute_script(
        "CREATE TABLE authors (id INTEGER PRIMARY KEY AUTOINCREMENT, name VARCHAR(100) NOT NULL);
         CREATE TABLE books (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL,
             author_id INTEGER NOT NULL REFERENCES authors(id) ON DELETE CASCADE, pages INTEGER);",
    )
    .await
    .unwrap();
    db
}

/// Minimal insert used until `save()` lands: the contract, not the API.
async fn insert<M: Model>(db: &Db, mut model: M) -> Result<M, OrmError> {
    let values: Vec<_> = model
        .to_values()
        .into_iter()
        .filter(|(col, _)| M::META.column(col).is_some_and(|f| !f.auto))
        .collect();
    let plan = WritePlan::Insert(InsertPlan {
        table: M::META.table.into(),
        columns: values.iter().map(|(c, _)| (*c).into()).collect(),
        rows: vec![values.into_iter().map(|(_, v)| v).collect()],
        returning: vec![M::META.pk().unwrap().column.into()],
    });
    let result = db.execute(&plan).await?;
    let pk = result.returning[0].iter().next().unwrap().1.clone();
    model.set_pk(pk)?;
    Ok(model)
}

fn author(name: &str) -> Author {
    Author {
        id: 0,
        name: name.into(),
    }
}

#[tokio::test]
async fn round_trip_through_meta_and_querysets() {
    let db = db().await;
    let ann = insert(&db, author("Ann")).await.unwrap();
    insert(&db, author("Zed")).await.unwrap();
    assert!(ann.id > 0);
    let book = insert(
        &db,
        Book {
            id: 0,
            title: "Rust".into(),
            author: ForeignKey::new(ann.id),
            pages: None,
        },
    )
    .await
    .unwrap();

    let names: Vec<_> = Author::objects(&db)
        .all()
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, ["Ann", "Zed"], "default ordering from META");

    let found = Author::objects(&db)
        .order_by([Author::name.desc()])
        .first()
        .await
        .unwrap();
    assert_eq!(found.map(|a| a.name).as_deref(), Some("Zed"));

    let books = ann.books(&db).all().await.unwrap();
    assert_eq!(books, [book]);
    assert_eq!(books[0].author.id(), &ann.id);
    assert_eq!(books[0].author.get(&db).await.unwrap().name, "Ann");

    let missing = Author::objects(&db).get(Author::id.eq(999_i64)).await;
    assert!(matches!(
        missing,
        Err(OrmError::Query(QueryError::DoesNotExist))
    ));
    let many = Author::objects(&db).get(Author::name.icontains("")).await;
    assert!(matches!(
        many,
        Err(OrmError::Query(QueryError::MultipleObjectsReturned(2)))
    ));
}

#[tokio::test]
async fn transactions_commit_roll_back_and_nest() {
    let db = db().await;
    let committed = Arc::new(AtomicUsize::new(0));

    let hook = Arc::clone(&committed);
    db.transaction(|tx| async move {
        insert(&tx, author("kept")).await?;
        tx.on_commit(move || {
            hook.fetch_add(1, Ordering::SeqCst);
        });
        // Savepoint that fails: only its work is undone, its hook discarded.
        let inner: Result<(), OrmError> = tx
            .transaction(|sp| async move {
                insert(&sp, author("undone")).await?;
                sp.on_commit(|| panic!("hook of a rolled-back savepoint ran"));
                Err(QueryError::InvalidPlan("boom".into()).into())
            })
            .await;
        assert!(inner.is_err());
        Ok::<_, OrmError>(())
    })
    .await
    .unwrap();
    assert_eq!(committed.load(Ordering::SeqCst), 1);

    let failed: Result<(), OrmError> = db
        .transaction(|tx| async move {
            insert(&tx, author("rolled back")).await?;
            Err(QueryError::InvalidPlan("abort".into()).into())
        })
        .await;
    assert!(failed.is_err());

    let names: Vec<_> = Author::objects(&db)
        .all()
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, ["kept"]);
}

#[tokio::test]
async fn unsupported_isolation_is_rejected_before_io() {
    let db = db().await;
    let result: Result<(), OrmError> = db
        .transaction_with(IsolationLevel::ReadCommitted, |_tx| async { Ok(()) })
        .await;
    assert!(matches!(
        result,
        Err(OrmError::Capability(
            siderite_orm::BackendCapabilityError::Unsupported {
                feature: Feature::Isolation(IsolationLevel::ReadCommitted),
                ..
            }
        ))
    ));
    db.transaction_with(IsolationLevel::Serializable, |_tx| async {
        Ok::<_, OrmError>(())
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn foreign_keys_are_enforced_and_decoded() {
    let db = db().await;
    let orphan = insert(
        &db,
        Book {
            id: 0,
            title: "Orphan".into(),
            author: ForeignKey::new(42),
            pages: Some(1),
        },
    )
    .await;
    assert!(
        matches!(
            orphan,
            Err(OrmError::Backend(siderite_orm::BackendError::Constraint(_)))
        ),
        "{orphan:?}"
    );
    assert_eq!(<Option<i32> as DbType>::SQL_TYPE, SqlType::Integer);
    let _ = Expr::col("x");
}
