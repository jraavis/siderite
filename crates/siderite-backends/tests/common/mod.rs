//! Hand-written models (the shape `#[derive(Model)]` generates) and a seeded
//! in-memory SQLite database shared by the end-to-end tests.
#![allow(clippy::unwrap_used, non_upper_case_globals, dead_code)]

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
#[cfg(feature = "sqlite")]
use siderite_backends::sqlite::SqliteBackend;
use siderite_orm::types::decode;
use siderite_orm::{
    Db, DbDefault, DbType, Field, FieldMeta, ForeignKey, ManyToManyManager, ManyToManyMeta, Model,
    ModelMeta, OnDelete, OrderDirection, QueryError, Relation, RelationKind, RelationMeta, Row,
    Value, read_column,
};
use uuid::Uuid;

/// Implements the pieces of `Model` that are identical for every test model.
macro_rules! model_boilerplate {
    ($pk:ident: $pk_ty:ty, unsaved = $unsaved:expr) => {
        fn pk(&self) -> $pk_ty {
            self.$pk.clone()
        }
        fn set_pk(&mut self, value: Value) -> Result<(), QueryError> {
            self.$pk = decode(stringify!($pk), value)?;
            Ok(())
        }
        fn is_unsaved(&self) -> bool {
            $unsaved(self)
        }
    };
}

pub mod cursor;

fn auto_pk() -> FieldMeta {
    FieldMeta {
        primary_key: true,
        auto: true,
        ..FieldMeta::new("id", "id", <i64 as DbType>::SQL_TYPE)
    }
}

fn plain<T: DbType>(name: &'static str) -> FieldMeta {
    FieldMeta {
        nullable: T::NULLABLE,
        ..FieldMeta::new(name, name, T::SQL_TYPE)
    }
}

// ---- Team -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Team {
    pub id: i64,
    pub name: String,
}

impl Team {
    pub const id: Field<Team, i64> = Field::new("id");
    pub const name: Field<Team, String> = Field::new("name");
}

static TEAM_META: ModelMeta = ModelMeta {
    name: "Team",
    table: "teams",
    fields: &[
        FieldMeta {
            primary_key: true,
            auto: true,
            ..FieldMeta::new("id", "id", siderite_orm::SqlType::BigInt)
        },
        FieldMeta::new("name", "name", siderite_orm::SqlType::Text),
    ],
    many_to_many: &[],
    ordering: &[],
    indexes: &[],
    constraints: &[],
    managed: true,
};

impl Model for Team {
    type Pk = i64;
    const META: &'static ModelMeta = &TEAM_META;
    model_boilerplate!(id: i64, unsaved = |s: &Team| s.id == 0);
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

// ---- Author -----------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Author {
    pub id: i64,
    pub name: String,
    pub age: Option<i32>,
    pub team: Option<ForeignKey<Team>>,
}

impl Author {
    pub const id: Field<Author, i64> = Field::new("id");
    pub const name: Field<Author, String> = Field::new("name");
    pub const age: Field<Author, Option<i32>> = Field::new("age");
    pub const team: Field<Author, Option<ForeignKey<Team>>> = Field::new("team_id");

    pub fn new(name: &str, age: Option<i32>) -> Self {
        Self {
            id: 0,
            name: name.into(),
            age,
            team: None,
        }
    }

    /// `select_related` / `prefetch_related` handle for `team`.
    pub fn team_relation() -> Relation<Author, Team> {
        Relation::new(Author::team, |a| &mut a.team)
    }

    pub fn books(&self, db: &Db) -> siderite_orm::QuerySet<Book> {
        Book::objects(db).filter(Book::author.eq(self.id))
    }
}

fn team_meta() -> &'static ModelMeta {
    Team::META
}

static AUTHOR_META: ModelMeta = ModelMeta {
    name: "Author",
    table: "authors",
    fields: &[
        FieldMeta {
            primary_key: true,
            auto: true,
            ..FieldMeta::new("id", "id", siderite_orm::SqlType::BigInt)
        },
        FieldMeta {
            max_length: Some(100),
            ..FieldMeta::new("name", "name", siderite_orm::SqlType::Text)
        },
        FieldMeta {
            nullable: true,
            ..FieldMeta::new("age", "age", siderite_orm::SqlType::Integer)
        },
        FieldMeta {
            nullable: true,
            relation: Some(RelationMeta {
                kind: RelationKind::ForeignKey,
                target: team_meta,
                on_delete: OnDelete::SetNull,
                related_name: Some("authors"),
            }),
            ..FieldMeta::new("team", "team_id", siderite_orm::SqlType::BigInt)
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
    model_boilerplate!(id: i64, unsaved = |s: &Author| s.id == 0);
    fn to_values(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("id", self.id.to_value()),
            ("name", self.name.to_value()),
            ("age", self.age.to_value()),
            ("team_id", self.team.to_value()),
        ]
    }
    fn from_row(row: &Row, prefix: &str) -> Result<Self, QueryError> {
        Ok(Self {
            id: read_column(row, prefix, "id")?,
            name: read_column(row, prefix, "name")?,
            age: read_column(row, prefix, "age")?,
            team: read_column(row, prefix, "team_id")?,
        })
    }
}

// ---- Tag (manual primary key) ------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub slug: String,
    pub label: String,
}

impl Tag {
    pub const slug: Field<Tag, String> = Field::new("slug");
    pub const label: Field<Tag, String> = Field::new("label");

    pub fn new(slug: &str, label: &str) -> Self {
        Self {
            slug: slug.into(),
            label: label.into(),
        }
    }
}

static TAG_META: ModelMeta = ModelMeta {
    name: "Tag",
    table: "tags",
    fields: &[
        FieldMeta {
            primary_key: true,
            ..FieldMeta::new("slug", "slug", siderite_orm::SqlType::Text)
        },
        FieldMeta::new("label", "label", siderite_orm::SqlType::Text),
    ],
    many_to_many: &[],
    ordering: &[("slug", OrderDirection::Asc)],
    indexes: &[],
    constraints: &[],
    managed: true,
};

impl Model for Tag {
    type Pk = String;
    const META: &'static ModelMeta = &TAG_META;
    model_boilerplate!(slug: String, unsaved = |_: &Tag| false);
    fn to_values(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("slug", self.slug.to_value()),
            ("label", self.label.to_value()),
        ]
    }
    fn from_row(row: &Row, prefix: &str) -> Result<Self, QueryError> {
        Ok(Self {
            slug: read_column(row, prefix, "slug")?,
            label: read_column(row, prefix, "label")?,
        })
    }
}

// ---- Book ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Book {
    pub id: i64,
    pub title: String,
    pub author: ForeignKey<Author>,
    pub pages: Option<i32>,
    pub price: Decimal,
    pub likes: i64,
    pub dislikes: i64,
    pub published: NaiveDate,
}

impl Book {
    pub const id: Field<Book, i64> = Field::new("id");
    pub const title: Field<Book, String> = Field::new("title");
    pub const author: Field<Book, ForeignKey<Author>> = Field::new("author_id");
    pub const pages: Field<Book, Option<i32>> = Field::new("pages");
    pub const price: Field<Book, Decimal> = Field::new("price");
    pub const likes: Field<Book, i64> = Field::new("likes");
    pub const dislikes: Field<Book, i64> = Field::new("dislikes");
    pub const published: Field<Book, NaiveDate> = Field::new("published");

    pub fn new(title: &str, author: i64) -> Self {
        Self {
            id: 0,
            title: title.into(),
            author: ForeignKey::new(author),
            pages: None,
            price: Decimal::new(1000, 2),
            likes: 0,
            dislikes: 0,
            published: NaiveDate::from_ymd_opt(2020, 1, 15).unwrap(),
        }
    }

    /// `select_related` / `prefetch_related` handle for `author`.
    pub fn author_relation() -> Relation<Book, Author> {
        Relation::new(Book::author, |b| &mut b.author)
    }

    pub fn tags(&self, db: &Db) -> ManyToManyManager<Book, Tag> {
        ManyToManyManager::new(db, &BOOK_META.many_to_many[0], self.id.to_value())
    }
}

fn author_meta() -> &'static ModelMeta {
    Author::META
}
fn tag_meta() -> &'static ModelMeta {
    Tag::META
}

static BOOK_META: ModelMeta = ModelMeta {
    name: "Book",
    table: "books",
    fields: &[
        FieldMeta {
            primary_key: true,
            auto: true,
            ..FieldMeta::new("id", "id", siderite_orm::SqlType::BigInt)
        },
        FieldMeta::new("title", "title", siderite_orm::SqlType::Text),
        FieldMeta {
            index: true,
            relation: Some(RelationMeta {
                kind: RelationKind::ForeignKey,
                target: author_meta,
                on_delete: OnDelete::Cascade,
                related_name: Some("books"),
            }),
            ..FieldMeta::new("author", "author_id", siderite_orm::SqlType::BigInt)
        },
        FieldMeta {
            nullable: true,
            ..FieldMeta::new("pages", "pages", siderite_orm::SqlType::Integer)
        },
        FieldMeta {
            max_digits: Some(8),
            decimal_places: Some(2),
            ..FieldMeta::new("price", "price", siderite_orm::SqlType::Decimal)
        },
        FieldMeta::new("likes", "likes", siderite_orm::SqlType::BigInt),
        FieldMeta::new("dislikes", "dislikes", siderite_orm::SqlType::BigInt),
        FieldMeta::new("published", "published", siderite_orm::SqlType::Date),
    ],
    many_to_many: &[ManyToManyMeta {
        name: "tags",
        target: tag_meta,
        through_table: "book_tags",
        source_column: "book_id",
        target_column: "tag_slug",
        through: None,
        related_name: None,
    }],
    ordering: &[],
    indexes: &[],
    constraints: &[],
    managed: true,
};

impl Model for Book {
    type Pk = i64;
    const META: &'static ModelMeta = &BOOK_META;
    model_boilerplate!(id: i64, unsaved = |s: &Book| s.id == 0);
    fn to_values(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("id", self.id.to_value()),
            ("title", self.title.to_value()),
            ("author_id", self.author.to_value()),
            ("pages", self.pages.to_value()),
            ("price", self.price.to_value()),
            ("likes", self.likes.to_value()),
            ("dislikes", self.dislikes.to_value()),
            ("published", self.published.to_value()),
        ]
    }
    fn from_row(row: &Row, prefix: &str) -> Result<Self, QueryError> {
        Ok(Self {
            id: read_column(row, prefix, "id")?,
            title: read_column(row, prefix, "title")?,
            author: read_column(row, prefix, "author_id")?,
            pages: read_column(row, prefix, "pages")?,
            price: read_column(row, prefix, "price")?,
            likes: read_column(row, prefix, "likes")?,
            dislikes: read_column(row, prefix, "dislikes")?,
            published: read_column(row, prefix, "published")?,
        })
    }
}

// ---- Note (auto_now_add / auto_now) --------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub id: i64,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Note {
    pub const id: Field<Note, i64> = Field::new("id");
    pub const body: Field<Note, String> = Field::new("body");
    pub const created_at: Field<Note, DateTime<Utc>> = Field::new("created_at");
    pub const updated_at: Field<Note, DateTime<Utc>> = Field::new("updated_at");

    pub fn new(body: &str) -> Self {
        Self {
            id: 0,
            body: body.into(),
            created_at: DateTime::UNIX_EPOCH,
            updated_at: DateTime::UNIX_EPOCH,
        }
    }
}

static NOTE_META: ModelMeta = ModelMeta {
    name: "Note",
    table: "notes",
    fields: &[
        FieldMeta {
            primary_key: true,
            auto: true,
            ..FieldMeta::new("id", "id", siderite_orm::SqlType::BigInt)
        },
        FieldMeta::new("body", "body", siderite_orm::SqlType::Text),
        FieldMeta {
            default: Some(DbDefault::Now),
            ..FieldMeta::new("created_at", "created_at", siderite_orm::SqlType::Timestamp)
        },
        FieldMeta {
            auto_now: true,
            ..FieldMeta::new("updated_at", "updated_at", siderite_orm::SqlType::Timestamp)
        },
    ],
    many_to_many: &[],
    ordering: &[],
    indexes: &[],
    constraints: &[],
    managed: true,
};

impl Model for Note {
    type Pk = i64;
    const META: &'static ModelMeta = &NOTE_META;
    model_boilerplate!(id: i64, unsaved = |s: &Note| s.id == 0);
    fn to_values(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("id", self.id.to_value()),
            ("body", self.body.to_value()),
            ("created_at", self.created_at.to_value()),
            ("updated_at", self.updated_at.to_value()),
        ]
    }
    fn from_row(row: &Row, prefix: &str) -> Result<Self, QueryError> {
        Ok(Self {
            id: read_column(row, prefix, "id")?,
            body: read_column(row, prefix, "body")?,
            created_at: read_column(row, prefix, "created_at")?,
            updated_at: read_column(row, prefix, "updated_at")?,
        })
    }
}

// ---- Event (uuid key, every scalar type) ----------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub id: Uuid,
    pub at: DateTime<Utc>,
    pub day: NaiveDate,
    pub clock: NaiveTime,
    pub payload: serde_json::Value,
    pub blob: Vec<u8>,
    pub score: Option<f64>,
}

impl Event {
    pub const id: Field<Event, Uuid> = Field::new("id");
    pub const at: Field<Event, DateTime<Utc>> = Field::new("at");
    pub const day: Field<Event, NaiveDate> = Field::new("day");
    pub const clock: Field<Event, NaiveTime> = Field::new("clock");
    pub const score: Field<Event, Option<f64>> = Field::new("score");
}

static EVENT_META: ModelMeta = ModelMeta {
    name: "Event",
    table: "events",
    fields: &[
        FieldMeta {
            primary_key: true,
            ..FieldMeta::new("id", "id", siderite_orm::SqlType::Uuid)
        },
        FieldMeta::new("at", "at", siderite_orm::SqlType::Timestamp),
        FieldMeta::new("day", "day", siderite_orm::SqlType::Date),
        FieldMeta::new("clock", "clock", siderite_orm::SqlType::Time),
        FieldMeta::new("payload", "payload", siderite_orm::SqlType::Json),
        FieldMeta::new("blob", "blob", siderite_orm::SqlType::Binary),
        FieldMeta {
            nullable: true,
            ..FieldMeta::new("score", "score", siderite_orm::SqlType::Double)
        },
    ],
    many_to_many: &[],
    ordering: &[],
    indexes: &[],
    constraints: &[],
    managed: true,
};

impl Model for Event {
    type Pk = Uuid;
    const META: &'static ModelMeta = &EVENT_META;
    model_boilerplate!(id: Uuid, unsaved = |_: &Event| false);
    fn to_values(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("id", self.id.to_value()),
            ("at", self.at.to_value()),
            ("day", self.day.to_value()),
            ("clock", self.clock.to_value()),
            ("payload", self.payload.to_value()),
            ("blob", self.blob.to_value()),
            ("score", self.score.to_value()),
        ]
    }
    fn from_row(row: &Row, prefix: &str) -> Result<Self, QueryError> {
        Ok(Self {
            id: read_column(row, prefix, "id")?,
            at: read_column(row, prefix, "at")?,
            day: read_column(row, prefix, "day")?,
            clock: read_column(row, prefix, "clock")?,
            payload: read_column(row, prefix, "payload")?,
            blob: read_column(row, prefix, "blob")?,
            score: read_column(row, prefix, "score")?,
        })
    }
}

// ---- database -------------------------------------------------------------

pub const SCHEMA: &str = "
    CREATE TABLE teams (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);
    CREATE TABLE authors (
        id INTEGER PRIMARY KEY AUTOINCREMENT, name VARCHAR(100) NOT NULL, age INTEGER,
        team_id INTEGER REFERENCES teams(id) ON DELETE SET NULL);
    CREATE TABLE tags (slug TEXT PRIMARY KEY, label TEXT NOT NULL);
    CREATE TABLE books (
        id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL,
        author_id INTEGER NOT NULL REFERENCES authors(id) ON DELETE CASCADE,
        pages INTEGER, price NUMERIC NOT NULL, likes INTEGER NOT NULL, dislikes INTEGER NOT NULL,
        published TEXT NOT NULL);
    CREATE TABLE book_tags (
        book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
        tag_slug TEXT NOT NULL REFERENCES tags(slug) ON DELETE CASCADE,
        PRIMARY KEY (book_id, tag_slug));
    CREATE TABLE notes (
        id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL,
        created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
    CREATE TABLE events (
        id TEXT PRIMARY KEY, at TEXT NOT NULL, day TEXT NOT NULL, clock TEXT NOT NULL,
        payload TEXT NOT NULL, blob BLOB NOT NULL, score REAL);
";

/// Empty in-memory database with every test table.
#[cfg(feature = "sqlite")]
pub async fn db() -> Db {
    let db = Db::new(SqliteBackend::connect("sqlite::memory:").await.unwrap());
    db.execute_script(SCHEMA).await.unwrap();
    db
}

/// Rows created by [`seed`].
pub struct Seed {
    pub red: Team,
    pub blue: Team,
    pub ann: Author,
    pub bob: Author,
    pub cy: Author,
    pub dee: Author,
    pub books: Vec<Book>,
}

impl Seed {
    /// The seeded book called `title`.
    pub fn book(&self, title: &str) -> &Book {
        self.books.iter().find(|b| b.title == title).unwrap()
    }
}

/// Two teams, four authors (ages 30, 25, none, 41) and five books.
pub async fn seed(db: &Db) -> Seed {
    use siderite_orm::ModelOps;
    async fn saved<T: Model + Send>(db: &Db, mut model: T) -> T {
        model.save(db).await.unwrap();
        model
    }
    let red = saved(
        db,
        Team {
            id: 0,
            name: "Red".into(),
        },
    )
    .await;
    let blue = saved(
        db,
        Team {
            id: 0,
            name: "Blue".into(),
        },
    )
    .await;
    let author = |name: &str, age: Option<i32>, team: Option<&Team>| Author {
        team: team.map(|t| ForeignKey::new(t.id)),
        ..Author::new(name, age)
    };
    let ann = saved(db, author("Ann", Some(30), Some(&red))).await;
    let bob = saved(db, author("Bob", Some(25), Some(&blue))).await;
    let cy = saved(db, author("Cy", None, None)).await;
    let dee = saved(db, author("Dee", Some(41), Some(&red))).await;
    let book = |title: &str,
                by: &Author,
                likes,
                dislikes,
                pages,
                price: (i64, u32),
                published: (i32, u32, u32)| Book {
        likes,
        dislikes,
        pages,
        price: Decimal::new(price.0, price.1),
        published: NaiveDate::from_ymd_opt(published.0, published.1, published.2).unwrap(),
        ..Book::new(title, by.id)
    };
    let mut books = Vec::new();
    for b in [
        book("Rust", &ann, 10, 2, Some(300), (3000, 2), (2020, 3, 15)),
        book("Async", &ann, 5, 5, None, (1250, 2), (2021, 7, 1)),
        book("SQL", &bob, 8, 1, Some(150), (2000, 2), (2019, 12, 31)),
        book("Go", &dee, 1, 0, Some(200), (4599, 2), (2022, 11, 20)),
        book("Zig", &dee, 3, 3, Some(120), (999, 2), (2022, 2, 2)),
    ] {
        books.push(saved(db, b).await);
    }
    Seed {
        red,
        blue,
        ann,
        bob,
        cy,
        dee,
        books,
    }
}

/// Titles of `books`, in order.
pub fn titles(books: &[Book]) -> Vec<&str> {
    books.iter().map(|b| b.title.as_str()).collect()
}
