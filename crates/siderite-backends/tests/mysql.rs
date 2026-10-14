//! MySQL end-to-end tests.
//!
//! These tests are explicitly ignored in offline runs. Select --ignored
//! with the documented service URL to run them; missing configuration fails.
//! Each test owns a disposable schema/database or a unique Redis namespace.
//! Shared application data and Redis FLUSH commands are not used.
#![cfg(feature = "mysql")]
#![allow(clippy::unwrap_used)]

mod common;

use chrono::{DateTime, NaiveTime, TimeDelta, Utc};
use common::{Author, Book, Event, Note, Tag, Team, seed, titles};
use siderite_backends::mysql::MySqlBackend;
use siderite_orm::functions::{case, concat, length, upper};
use siderite_orm::{ArrayAgg, BackendCapabilityError, BackendError, Count};
use siderite_orm::{Db, Expr, IsolationLevel, Model, ModelOps, OrmError};
use siderite_orm::{QueryError, QuerySet, Relation, RowNumber, StdDev};
use siderite_orm::{StringAgg, Sum, Value, Variance, params};
use sqlx::MySqlPool;
use sqlx::mysql::MySqlPoolOptions;

const SCHEMA: &str = "
    CREATE TABLE teams (
        id BIGINT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(191) NOT NULL);
    CREATE TABLE authors (
        id BIGINT AUTO_INCREMENT PRIMARY KEY,
        name VARCHAR(100) NOT NULL, age INT,
        team_id BIGINT NULL,
        FOREIGN KEY (team_id) REFERENCES teams(id) ON DELETE SET NULL);
    CREATE TABLE tags (
        slug VARCHAR(191) PRIMARY KEY, label VARCHAR(191) NOT NULL);
    CREATE TABLE books (
        id BIGINT AUTO_INCREMENT PRIMARY KEY, title VARCHAR(191) NOT NULL,
        author_id BIGINT NOT NULL,
        pages INT, price DECIMAL(8,2) NOT NULL, likes BIGINT NOT NULL,
        dislikes BIGINT NOT NULL, published DATE NOT NULL,
        FOREIGN KEY (author_id) REFERENCES authors(id) ON DELETE CASCADE);
    CREATE TABLE book_tags (
        book_id BIGINT NOT NULL, tag_slug VARCHAR(191) NOT NULL,
        PRIMARY KEY (book_id, tag_slug),
        FOREIGN KEY (book_id) REFERENCES books(id) ON DELETE CASCADE,
        FOREIGN KEY (tag_slug) REFERENCES tags(slug) ON DELETE CASCADE);
    CREATE TABLE notes (
        id BIGINT AUTO_INCREMENT PRIMARY KEY, body TEXT NOT NULL,
        created_at DATETIME(6) NOT NULL, updated_at DATETIME(6) NOT NULL);
    CREATE TABLE events (
        id CHAR(36) PRIMARY KEY, at DATETIME(6) NOT NULL, day DATE NOT NULL,
        clock TIME(6) NOT NULL, payload JSON NOT NULL,
        `blob` BLOB NOT NULL, score DOUBLE);
    CREATE TABLE nullables (
        id BIGINT AUTO_INCREMENT PRIMARY KEY, t TEXT, u CHAR(36),
        ts DATETIME(6), j JSON,
        n DECIMAL(20,4), d DATE);
";

/// Isolation level and state of the current connection's transaction.
const TRANSACTION_LEVEL: &str = "SELECT \
    CAST(isolation_level AS CHAR) AS level, CAST(state AS CHAR) AS state \
    FROM performance_schema.events_transactions_current \
    WHERE thread_id = PS_CURRENT_THREAD_ID()";

#[path = "mysql/fixture.rs"]
mod fixture;
#[path = "mysql/returning.rs"]
mod returning;
use fixture::TestDb;

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn session_is_utc_and_group_concat_is_not_truncated() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let rows =
        t.db.raw_sql(
            "SELECT @@session.time_zone AS tz, \
             @@session.group_concat_max_len AS len, \
             @@session.sql_mode AS mode",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(rows.rows[0].get_as::<String>("tz").unwrap(), "+00:00");
    assert!(rows.rows[0].get_as::<i64>("len").unwrap() > 1 << 20);
    assert!(
        !rows.rows[0]
            .get_as::<String>("mode")
            .unwrap()
            .contains("NO_BACKSLASH_ESCAPES")
    );

    let s = seed(&t.db).await;
    // 8 x 151 bytes plus separators is past the server default of 1024.
    let long = "x".repeat(150);
    for i in 0..8 {
        Book::new(&format!("{long}{i}"), s.ann.id)
            .save(&t.db)
            .await
            .unwrap();
    }
    let joined = Book::objects(&t.db)
        .filter(Book::title.starts_with("xxx"))
        .aggregate([("all", StringAgg::of(Book::title, ","))])
        .await
        .unwrap();
    assert_eq!(joined.get_as::<String>("all").unwrap().len(), 8 * 151 + 7);
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn instance_operations_and_timestamps() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let mut note = Note::new("hello");
    note.save(db).await.unwrap();
    assert!(note.id > 0 && note.created_at > chrono::DateTime::UNIX_EPOCH);
    assert!(note.created_at <= Utc::now() + TimeDelta::seconds(1));
    note.body = "edited".into();
    note.save(db).await.unwrap();
    let mut copy = Note::new("stale");
    copy.id = note.id;
    copy.refresh(db).await.unwrap();
    assert_eq!(copy.body, "edited");
    assert_eq!(note.delete(db).await.unwrap(), 1);
    assert_eq!(note.delete(db).await.unwrap(), 0);

    let mut without_age = Author::new("first", None);
    without_age.save(db).await.unwrap();
    let mut with_age = Author::new("second", Some(3));
    with_age.save(db).await.unwrap();
    assert_eq!(with_age.id, without_age.id + 1);
    without_age.age = Some(4);
    without_age.save(db).await.unwrap();
    with_age.age = None;
    with_age.save(db).await.unwrap();
    let ages: Vec<Option<i32>> = Author::objects(db)
        .order_by([Author::id.asc()])
        .values_list(["age"])
        .await
        .unwrap();
    assert_eq!(ages, [Some(4), None]);

    // Unchanged values still count as matched rows, so `save` finds the row.
    let mut same = Author::new("second", None);
    same.id = with_age.id;
    same.save(db).await.unwrap();
    assert_eq!(same.id, with_age.id);
    assert_eq!(Author::objects(db).count().await.unwrap(), 2);
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn manual_keys_are_returned_as_supplied() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let mut tag = Tag::new("rust", "Rust");
    tag.save(db).await.unwrap();
    tag.label = "Rust!".into();
    tag.save(db).await.unwrap();
    assert_eq!(Tag::objects(db).count().await.unwrap(), 1);
    assert_eq!(
        Tag::objects(db)
            .get(Tag::slug.eq("rust"))
            .await
            .unwrap()
            .label,
        "Rust!"
    );

    // Bulk insert of saved keys keeps the input order, not the key order.
    let tags = ["zeta", "alpha", "mid"]
        .iter()
        .map(|slug| Tag::new(slug, slug))
        .collect::<Vec<_>>();
    let stored = Tag::objects(db).bulk_create(tags).await.unwrap();
    assert_eq!(
        stored.iter().map(|t| t.slug.as_str()).collect::<Vec<_>>(),
        ["zeta", "alpha", "mid"]
    );
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn null_parameters_fit_any_column_type() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let inserted =
        t.db.raw_execute(
            "INSERT INTO nullables (t, u, ts, j, n, d) \
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null
            ],
        )
        .await
        .unwrap();
    assert_eq!(inserted, 1);
    let rows =
        t.db.raw_sql("SELECT t, u, ts, j, n, d FROM nullables", vec![])
            .await
            .unwrap();
    assert!(rows.rows[0].iter().all(|(_, v)| v.is_null()));
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn scalar_types_round_trip_natively() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let mut event = Event {
        id: uuid::Uuid::from_u128(0xfeed),
        at: DateTime::from_timestamp_micros(1_715_953_530_123_456).unwrap(),
        day: chrono::NaiveDate::from_ymd_opt(2024, 5, 17).unwrap(),
        clock: NaiveTime::from_hms_micro_opt(13, 45, 30, 123_456).unwrap(),
        payload: serde_json::json!({"k": [1, 2]}),
        blob: vec![0, 1, 255],
        score: None,
    };
    event.save(db).await.unwrap();
    assert_eq!(
        Event::objects(db)
            .get(Event::id.eq(event.id))
            .await
            .unwrap(),
        event
    );
    // The canonical storage of a Uuid is CHAR(36) text.
    let raw = t.db.raw_sql("SELECT id FROM events", vec![]).await.unwrap();
    assert_eq!(
        raw.rows[0].get("id"),
        Some(&Value::Text(event.id.hyphenated().to_string()))
    );
    let rows = Event::objects(db)
        .project([
            ("year", Event::at.year()),
            ("quarter", Event::at.quarter()),
            ("week", Event::at.week()),
            ("day", Event::at.day()),
            ("hour", Event::at.hour()),
            ("minute", Event::at.minute()),
            ("second", Event::at.second()),
            ("date", Event::at.date()),
        ])
        .rows()
        .await
        .unwrap();
    assert_eq!(rows[0].get_as::<i64>("year").unwrap(), 2024);
    assert_eq!(rows[0].get_as::<i64>("quarter").unwrap(), 2);
    assert_eq!(rows[0].get_as::<i64>("week").unwrap(), 20);
    assert_eq!(rows[0].get_as::<i64>("day").unwrap(), 17);
    assert_eq!(rows[0].get_as::<i64>("hour").unwrap(), 13);
    assert_eq!(rows[0].get_as::<i64>("minute").unwrap(), 45);
    assert_eq!(rows[0].get_as::<i64>("second").unwrap(), 30);
    assert_eq!(
        rows[0].get_as::<chrono::NaiveDate>("date").unwrap(),
        event.day
    );
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn queries_aggregates_windows_and_sets() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let s = seed(db).await;
    let liked = Book::objects(db)
        .filter(Book::likes.ge(5_i64))
        .order_by([Book::id.asc()]);
    assert_eq!(
        titles(&liked.clone().all().await.unwrap()),
        ["Rust", "Async", "SQL"]
    );
    assert_eq!(Book::objects(db).limit(2).count().await.unwrap(), 2);
    // OFFSET without LIMIT needs MySQL's unbounded LIMIT.
    let tail = Book::objects(db)
        .order_by([Book::id.asc()])
        .offset(3)
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&tail), ["Go", "Zig"]);
    assert_eq!(
        Book::objects(db).paginate(2, 2).await.unwrap().items.len(),
        2
    );

    let likes = Sum::of(Book::likes);
    let totals = [("likes", likes), ("n", Sum::of(Book::price))];
    let row = Book::objects(db).aggregate(totals).await.unwrap();
    assert_eq!(
        row.get_as::<i64>("likes").unwrap(),
        27,
        "DECIMAL sums decode as integers"
    );
    let stats = Book::objects(db)
        .aggregate([
            ("pop", StdDev::population(Book::likes)),
            ("samp", StdDev::sample(Book::likes)),
            ("var", Variance::sample(Book::likes)),
        ])
        .await
        .unwrap();
    // likes = 10, 5, 8, 1, 3: mean 5.4, sum of squared deviations 53.2.
    let population = stats.get_as::<f64>("pop").unwrap();
    let sample = stats.get_as::<f64>("samp").unwrap();
    assert!((population - (53.2_f64 / 5.0).sqrt()).abs() < 1e-9);
    assert!((sample - (53.2_f64 / 4.0).sqrt()).abs() < 1e-9);
    assert!((stats.get_as::<f64>("var").unwrap() - 53.2 / 4.0).abs() < 1e-9);

    let grouped = Book::objects(db)
        .project([Book::author.select()])
        .annotate("books", Count::all())
        .filter(Expr::col("books").gt(1_i64))
        .order_by([Book::author.asc()])
        .values_list::<(i64, i64), _>(["author_id", "books"])
        .await
        .unwrap();
    assert_eq!(grouped, [(s.ann.id, 2), (s.dee.id, 2)]);

    // FILTER (WHERE ..) is rewritten into the aggregate's argument.
    let filtered = Book::objects(db)
        .aggregate([
            ("popular", Count::all().filter(Book::likes.ge(5_i64))),
            (
                "popular_likes",
                Sum::of(Book::likes).filter(Book::likes.ge(5_i64)),
            ),
        ])
        .await
        .unwrap();
    assert_eq!(filtered.get_as::<i64>("popular").unwrap(), 3);
    assert_eq!(filtered.get_as::<i64>("popular_likes").unwrap(), 23);

    let ranked = Book::objects(db)
        .annotate(
            "rn",
            RowNumber::new()
                .partition_by([Book::author])
                .order_by([Book::likes.desc()]),
        )
        .annotate(
            "heat",
            case().when(Book::likes.ge(8_i64), "hot").otherwise("cold"),
        )
        .order_by([Book::id.asc()])
        .all_annotated()
        .await
        .unwrap();
    assert_eq!(ranked[1].1.get_as::<i64>("rn").unwrap(), 2);
    assert_eq!(ranked[2].1.get_as::<String>("heat").unwrap(), "hot");

    let others = Book::objects(db).filter(Book::title.eq("Go"));
    let union = liked.clone().union(others.clone()).unwrap();
    assert_eq!(union.count().await.unwrap(), 4);
    let common = liked
        .clone()
        .intersection(Book::objects(db).filter(Book::pages.gt(140)))
        .unwrap();
    assert_eq!(
        titles(&common.order_by([Book::id.asc()]).all().await.unwrap()),
        ["Rust", "SQL"]
    );
    let except = liked.clone().difference(others).unwrap();
    assert_eq!(except.count().await.unwrap(), 3);
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn text_lookups_are_case_sensitive_when_asked() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let s = seed(db).await;
    let names = [
        "Hello",
        "hello",
        "50%_off",
        r"back\slash",
        "it's",
        "ünïcode",
        "  pad  ",
    ];
    for name in names {
        Book::new(name, s.ann.id).save(db).await.unwrap();
    }
    let count = |q: QuerySet<Book>| async move { q.count().await.unwrap() };

    // The default collation is case-insensitive, so `eq` follows it; the
    // explicit lookups are case-sensitive.
    assert_eq!(
        count(Book::objects(db).filter(Book::title.contains("ell"))).await,
        2
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.contains("ELL"))).await,
        0
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.icontains("ELL"))).await,
        2
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.starts_with("h"))).await,
        1
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.starts_with("H"))).await,
        1
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.istarts_with("H"))).await,
        2
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.ends_with("LO"))).await,
        0
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.iends_with("LO"))).await,
        2
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.iexact("HELLO"))).await,
        2
    );
    // Wildcards and the escape character in the needle are literal.
    assert_eq!(
        count(Book::objects(db).filter(Book::title.contains("0%_"))).await,
        1
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.contains("%"))).await,
        1
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.contains("_"))).await,
        1
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.contains(r"k\s"))).await,
        1
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.contains("\\"))).await,
        1
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.contains("it's"))).await,
        1
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.contains("Ünï"))).await,
        0
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.icontains("ÜNÏ"))).await,
        1
    );
    // Regex is case-sensitive, whatever the collation.
    assert_eq!(
        count(Book::objects(db).filter(Book::title.regex("^h"))).await,
        1
    );
    assert_eq!(
        count(Book::objects(db).filter(Book::title.regex("^[Hh]ello$"))).await,
        2
    );

    // NULL counts as empty text in CONCAT, LENGTH counts characters.
    let parts = [Book::title.expr(), Book::pages.expr(), Expr::val("!")];
    let rows = Book::objects(db)
        .filter(Book::title.eq("ünïcode"))
        .project([
            ("joined", concat(parts)),
            ("len", length(Book::title.expr())),
            ("up", upper(Book::title.expr())),
        ])
        .rows()
        .await
        .unwrap();
    assert_eq!(rows[0].get_as::<String>("joined").unwrap(), "ünïcode!");
    assert_eq!(rows[0].get_as::<i64>("len").unwrap(), 7);

    // StringAgg: quotes and backslashes in the separator are literal.
    let agg = Book::objects(db)
        .filter(Book::title.istarts_with("hello"))
        .aggregate([("t", StringAgg::of(Book::title, r"'\|"))])
        .await
        .unwrap();
    let joined = agg.get_as::<String>("t").unwrap();
    assert_eq!(joined.matches(r"'\|").count(), 1, "{joined}");
    let distinct = Book::objects(db)
        .filter(Book::title.istarts_with("hello"))
        .aggregate([("t", StringAgg::of(Book::title, ",").distinct())])
        .await
        .unwrap();
    // DISTINCT follows collation: "Hello" and "hello" are one value.
    assert_eq!(
        distinct.get_as::<String>("t").unwrap().matches(',').count(),
        0
    );
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn unsupported_features_fail_before_any_io() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    seed(db).await;
    let distinct_on = Book::objects(db)
        .order_by([Book::author.asc()])
        .distinct_on([Book::author])
        .all()
        .await;
    assert!(matches!(
        distinct_on,
        Err(OrmError::Capability(
            BackendCapabilityError::Unsupported { .. }
        ))
    ));
    let array = Book::objects(db)
        .aggregate([("titles", ArrayAgg::of(Book::title))])
        .await;
    assert!(matches!(array, Err(OrmError::Capability(_))));
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn writes_bulk_operations_and_relations() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let s = seed(db).await;
    let many: Vec<Book> = (0..10_000)
        .map(|i| Book::new(&format!("b{i}"), s.ann.id))
        .collect();
    let stored = Book::objects(db).bulk_create(many).await.unwrap();
    assert_eq!(stored.len(), 10_000);
    for (i, book) in stored.iter().enumerate() {
        assert_eq!(book.title, format!("b{i}"), "input order is kept");
    }
    assert!(stored.windows(2).all(|w| w[1].id == w[0].id + 1));
    let changed = Book::objects(db)
        .bulk_update(&stored[..3], &["likes"])
        .await
        .unwrap();
    assert_eq!(changed, 3, "matched rows are counted, changed or not");

    // UPDATE / DELETE whose key subquery reads the target table and has a
    // LIMIT: MySQL rejects both unless the subquery is a derived table.
    let updated = Book::objects(db)
        .filter(Book::title.starts_with("b"))
        .order_by([Book::id.asc()])
        .limit(5)
        .update([Book::likes.set(7_i64)])
        .await
        .unwrap();
    assert_eq!(updated, 5);
    assert_eq!(
        Book::objects(db)
            .filter(Book::likes.eq(7_i64))
            .count()
            .await
            .unwrap(),
        5
    );
    let removed = Book::objects(db)
        .filter(Book::title.starts_with("b"))
        .order_by([Book::id.asc()])
        .offset(2)
        .limit(4)
        .delete()
        .await
        .unwrap();
    assert_eq!(removed, 4);
    assert_eq!(
        Book::objects(db)
            .filter(Book::title.starts_with("b"))
            .delete()
            .await
            .unwrap(),
        10_000 - 4
    );

    // A scalar subquery over the table being updated.
    let max_likes = Book::objects(db)
        .aggregate([("m", siderite_orm::Max::of(Book::likes))])
        .await
        .unwrap()
        .get_as::<i64>("m")
        .unwrap();
    assert_eq!(max_likes, 10);

    let books = Book::objects(db)
        .select_related(Relation::new(Book::author, |b| &mut b.author))
        .filter(Book::author.join(Author::name).eq("Ann"))
        .all()
        .await
        .unwrap();
    assert!(books.iter().all(|b| b.author.cached().is_some()));

    Tag::new("rust", "Rust").save(db).await.unwrap();
    let book = s.book("Rust");
    book.tags(db).add_pks(["rust".to_owned()]).await.unwrap();
    assert_eq!(book.tags(db).count().await.unwrap(), 1);
    let (_, created) = Tag::objects(db)
        .get_or_create(Tag::slug.eq("db"), || Tag::new("db", "DB"))
        .await
        .unwrap();
    assert!(created);
    let orphan = Book::new("Orphan", 999_999).save(db).await;
    assert!(matches!(
        orphan,
        Err(OrmError::Backend(BackendError::Constraint(_)))
    ));
    // ON DELETE CASCADE / SET NULL follow the foreign keys.
    assert_eq!(s.dee.delete(db).await.unwrap(), 1);
    assert_eq!(
        Book::objects(db)
            .filter(Book::author.eq(s.dee.id))
            .count()
            .await
            .unwrap(),
        0
    );
    assert_eq!(Team::objects(db).count().await.unwrap(), 2);
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn correlated_and_limited_subqueries() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let s = seed(db).await;
    // IN (subquery with LIMIT) is rejected by MySQL unless wrapped.
    let top = Book::objects(db)
        .order_by([Book::likes.desc()])
        .limit(2)
        .subquery(Book::author);
    let authors = Author::objects(db)
        .filter(Author::id.in_subquery(top))
        .order_by([Author::id.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(
        authors.iter().map(|a| a.id).collect::<Vec<_>>(),
        [s.ann.id, s.bob.id]
    );
    // Correlated EXISTS.
    let with_books = Author::objects(db)
        .filter(
            Book::objects(db)
                .filter(Book::author.expr().eq(Author::id.outer_ref()))
                .exists_expr(),
        )
        .count()
        .await
        .unwrap();
    assert_eq!(with_books, 3);
    // Correlated subqueries in UPDATE and DELETE (over another table).
    let per_author = Book::objects(db)
        .filter(Book::author.expr().eq(Author::id.outer_ref()))
        .subquery(("n", Count::all()));
    let changed = Author::objects(db)
        .update([Author::age.set_expr(Expr::subquery(per_author))])
        .await
        .unwrap();
    assert_eq!(changed, 4);
    let ages: Vec<Option<i32>> = Author::objects(db)
        .order_by([Author::id.asc()])
        .values_list(["age"])
        .await
        .unwrap();
    assert_eq!(ages, [Some(2), Some(1), Some(0), Some(2)]);
    // A same-table subquery: update the authors that are the youngest.
    let youngest = Author::objects(db)
        .order_by([Author::age.asc(), Author::id.asc()])
        .limit(1)
        .subquery(Author::id);
    let renamed = Author::objects(db)
        .filter(Author::id.in_subquery(youngest))
        .update([Author::name.set("Youngest")])
        .await
        .unwrap();
    assert_eq!(renamed, 1);
    assert_eq!(
        Author::objects(db)
            .get(Author::name.eq("Youngest"))
            .await
            .unwrap()
            .id,
        s.cy.id
    );
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn transactions_savepoints_isolation_and_row_locks() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let s = seed(db).await;
    for (level, name) in [
        (IsolationLevel::ReadCommitted, "READ COMMITTED"),
        (IsolationLevel::RepeatableRead, "REPEATABLE READ"),
        (IsolationLevel::Serializable, "SERIALIZABLE"),
    ] {
        let seen = db
            .transaction_with(level, |tx| async move {
                // `@@transaction_isolation` is the session default, so ask the
                // performance schema for the level of the running transaction.
                let rows = tx.raw_sql(TRANSACTION_LEVEL, vec![]).await?;
                let row = &rows.rows[0];
                assert_eq!(row.get_as::<String>("state").unwrap(), "ACTIVE");
                Ok::<_, OrmError>(row.get_as::<String>("level").unwrap())
            })
            .await
            .unwrap();
        assert_eq!(seen, name);
    }
    // The next plain transaction is back on the server default.
    let plain = db
        .transaction(|tx| async move {
            let rows = tx.raw_sql(TRANSACTION_LEVEL, vec![]).await?;
            Ok::<_, OrmError>(rows.rows[0].get_as::<String>("level").unwrap())
        })
        .await
        .unwrap();
    assert_eq!(plain, "REPEATABLE READ");

    // Savepoints: the inner rollback keeps the outer write.
    let kept = db
        .transaction(|tx| async move {
            Author::new("outer", None).save(&tx).await?;
            let inner: Result<(), OrmError> = tx
                .transaction(|inner| async move {
                    Author::new("inner", None).save(&inner).await?;
                    Err(QueryError::InvalidPlan("abort".into()).into())
                })
                .await;
            assert!(inner.is_err());
            Author::new("after", None).save(&tx).await?;
            Ok::<_, OrmError>(())
        })
        .await;
    kept.unwrap();
    let mut names: Vec<String> = Author::objects(db)
        .filter(Author::age.is_null())
        .values_list(["name"])
        .await
        .unwrap();
    names.sort();
    assert_eq!(names, ["Cy", "after", "outer"]);

    let locked = db
        .transaction(|tx| async move {
            let rows = Author::objects(&tx)
                .filter(Author::id.eq(s.ann.id))
                .select_for_update()
                .all()
                .await?;
            // Another connection cannot lock the same row without waiting.
            let contender = Author::objects(db)
                .filter(Author::id.eq(s.ann.id))
                .select_for_update()
                .nowait()
                .all()
                .await;
            let skipped = Author::objects(db)
                .filter(Author::id.eq(s.ann.id))
                .select_for_update()
                .skip_locked()
                .all()
                .await?;
            Ok::<_, OrmError>((rows.len(), contender.is_err(), skipped.len()))
        })
        .await
        .unwrap();
    assert_eq!(locked, (1, true, 0));

    let failed: Result<(), OrmError> = db
        .transaction(|tx| async move {
            Author::new("temp", None).save(&tx).await?;
            Err(siderite_orm::QueryError::InvalidPlan("abort".into()).into())
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(
        Author::objects(db)
            .filter(Author::name.eq("temp"))
            .count()
            .await
            .unwrap(),
        0
    );
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn concurrent_bulk_creates_get_their_own_keys() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let s = seed(db).await;
    let batch = |prefix: &'static str| {
        let author = s.ann.id;
        async move {
            let books: Vec<Book> = (0..200)
                .map(|i| Book::new(&format!("{prefix}{i}"), author))
                .collect();
            Book::objects(db).bulk_create(books).await.unwrap()
        }
    };
    let (a, b, c) = tokio::join!(batch("a"), batch("b"), batch("c"));
    for (prefix, stored) in [("a", a), ("b", b), ("c", c)] {
        assert_eq!(stored.len(), 200);
        for (i, book) in stored.iter().enumerate() {
            assert_eq!(book.title, format!("{prefix}{i}"));
        }
        assert!(
            stored.windows(2).all(|w| w[1].id == w[0].id + 1),
            "one multi-row insert gets consecutive keys"
        );
    }
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn update_assignments_read_old_values() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let s = seed(db).await;
    // Standard SQL: every right-hand side sees the row before the update.
    Book::objects(db)
        .filter(Book::id.eq(s.book("Rust").id))
        .update([
            Book::likes.set_expr(Book::likes * 10_i64),
            Book::dislikes.set_expr(Book::likes.expr() + 1_i64),
            Book::pages.set_expr(Book::likes.expr()),
        ])
        .await
        .unwrap();
    let rust = Book::objects(db)
        .get(Book::id.eq(s.book("Rust").id))
        .await
        .unwrap();
    assert_eq!((rust.likes, rust.dislikes, rust.pages), (100, 11, Some(10)));
    // A swap cannot be ordered; it is refused before any I/O.
    let swap = Book::objects(db)
        .update([
            Book::likes.set_expr(Book::dislikes.expr()),
            Book::dislikes.set_expr(Book::likes.expr()),
        ])
        .await;
    assert!(matches!(swap, Err(OrmError::Query(_))), "{swap:?}");
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn raw_sql_uses_question_mark_placeholders() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let inserted = db
        .raw_execute(
            "INSERT INTO teams (name) VALUES (?), (?)",
            params!["a", "b"],
        )
        .await
        .unwrap();
    assert_eq!(inserted, 2);
    let rows = db
        .raw_sql("SELECT name FROM teams WHERE name = ?", params!["b"])
        .await
        .unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert!(db.raw_sql("SELEC nope", vec![]).await.is_err());
    t.cleanup().await;
}

/// `update_or_create` locks its lookup (`FOR UPDATE`), and retries as an
/// update when a concurrent insert of the missing row wins.
#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn update_or_create_serializes_with_concurrent_writers() {
    use std::time::Duration;

    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let bump = |t: &mut Tag| {
        let value = t.label.parse::<i32>().unwrap() + 1;
        t.label = value.to_string();
    };
    // Runs `sql` in a transaction held open while `update_or_create` starts,
    // then bumps the `slug` row.
    let race = |sql: &'static str, slug: &'static str| async move {
        let holder = db.transaction(|tx| async move {
            tx.raw_execute(sql, vec![]).await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok::<_, OrmError>(())
        });
        let waiter = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let create = || Tag::new(slug, "unused");
            Tag::objects(db)
                .update_or_create(Tag::slug.eq(slug), create, bump)
                .await
        };
        let (held, outcome) = tokio::join!(holder, waiter);
        held.unwrap();
        outcome.unwrap()
    };

    // Without the lock the lookup would read "0" and overwrite the held "1".
    Tag::new("c", "0").save(db).await.unwrap();
    let update = "UPDATE tags SET label = '1' WHERE slug = 'c'";
    let (tag, created) = race(update, "c").await;
    assert!(!created);
    assert_eq!(tag.label, "2");

    // The row is missing; the held insert wins and the loser updates it.
    let insert = "INSERT INTO tags (slug, label) VALUES ('r', '1')";
    let (tag, created) = race(insert, "r").await;
    assert!(!created);
    assert_eq!(tag.label, "2");
    let stored = Tag::objects(db).get(Tag::slug.eq("r")).await.unwrap();
    assert_eq!(stored.label, "2");
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MYSQL_URL and an isolated live service"]
async fn cursor_pages_follow_the_contract_and_join_cleanly() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    db.execute_script(common::cursor::MYSQL_DDL).await.unwrap();
    common::cursor::run_contract(db, true).await;

    // Ordering columns shared with a joined table are qualified.
    seed(db).await;
    let joined = || {
        Book::objects(db)
            .select_related(Book::author_relation())
            .order_by([Book::likes.desc()])
    };
    let first = joined().cursor_page(3).await.unwrap();
    assert!(first.items.iter().all(|b| b.author.cached().is_some()));
    let rest = joined().after(&first.next.unwrap()).cursor_page(3);
    let rest = rest.await.unwrap();
    let all = joined().all().await.unwrap();
    assert_eq!([first.items, rest.items].concat(), all);
    t.cleanup().await;
}
