use crate::common::{Author, Book, Event, Note, titles};
use crate::fixtures::{TestDb, ms};
use chrono::TimeZone as _;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use siderite_orm::{Avg, Sum};
use siderite_orm::{Model as _, ModelOps as _};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn keys_are_generated_in_bulk_order_and_stored_as_id() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let mut a = Author::new("a", Some(1));
    a.save(db).await.unwrap();
    assert!(a.id > 0);
    let bulk = Author::objects(db)
        .bulk_create(
            (0..5)
                .map(|i| Author::new(&format!("n{i}"), Some(i)))
                .collect(),
        )
        .await
        .unwrap();
    let ids: Vec<i64> = bulk.iter().map(|x| x.id).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "{ids:?}");
    assert_eq!(
        bulk.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
        ["n0", "n1", "n2", "n3", "n4"]
    );
    assert!(ids[0] > a.id);
    // The key column lives in `_id`; there is no separate `id` field.
    let raw = t
        .backend
        .database()
        .collection::<mongodb::bson::Document>("authors")
        .find_one(mongodb::bson::doc! { "_id": a.id })
        .await
        .unwrap()
        .unwrap();
    assert!(!raw.contains_key("id"));
    assert_eq!(raw.get_str("name").unwrap(), "a");
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn instance_operations_and_timestamps() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let mut note = Note::new("hello");
    note.save(db).await.unwrap();
    assert!(note.id > 0 && note.created_at > DateTime::UNIX_EPOCH);
    assert_eq!(
        note.created_at,
        ms(note.created_at),
        "stored timestamps have millisecond precision"
    );
    note.body = "edited".into();
    note.save(db).await.unwrap();
    let mut copy = Note::new("stale");
    copy.id = note.id;
    copy.refresh(db).await.unwrap();
    assert_eq!(copy.body, "edited");
    assert_eq!(copy.created_at, note.created_at);
    assert_eq!(note.delete(db).await.unwrap(), 1);
    assert_eq!(note.delete(db).await.unwrap(), 0);
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn every_scalar_type_round_trips() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let mut e = Event {
        id: Uuid::new_v4(),
        at: Utc.timestamp_micros(1_700_000_000_123_456).unwrap(),
        day: NaiveDate::from_ymd_opt(2026, 9, 29).unwrap(),
        clock: NaiveTime::from_hms_micro_opt(13, 45, 6, 250_000).unwrap(),
        payload: serde_json::json!({"k": [1, 2, {"z": null}], "s": "x"}),
        blob: vec![0, 1, 254, 255],
        score: Some(2.5),
    };
    e.save(db).await.unwrap();
    assert_eq!(e.at, ms(e.at));
    let back = Event::objects(db).get(Event::id.eq(e.id)).await.unwrap();
    assert_eq!(back, e);
    assert_eq!(back.at.timestamp_millis(), 1_700_000_000_123);
    // Stored natively: uuid binary subtype 4, datetime, embedded document.
    let raw = t
        .backend
        .database()
        .collection::<mongodb::bson::Document>("events")
        .find_one(mongodb::bson::doc! {})
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        raw.get("_id"),
        Some(mongodb::bson::Bson::Binary(b)) if b.subtype == mongodb::bson::spec::BinarySubtype::Uuid
    ));
    assert!(matches!(
        raw.get("at"),
        Some(mongodb::bson::Bson::DateTime(_))
    ));
    assert!(matches!(
        raw.get("payload"),
        Some(mongodb::bson::Bson::Document(_))
    ));
    assert_eq!(raw.get_str("day").unwrap(), "2026-09-29");

    // Filtering on typed values.
    let n = Event::objects(db)
        .filter(Event::at.ge(e.at))
        .filter(Event::day.eq(e.day))
        .filter(Event::score.is_not_null())
        .count()
        .await
        .unwrap();
    assert_eq!(n, 1);

    let mut none = Event {
        score: None,
        id: Uuid::new_v4(),
        ..e.clone()
    };
    none.save(db).await.unwrap();
    assert_eq!(
        Event::objects(db)
            .get(Event::id.eq(none.id))
            .await
            .unwrap()
            .score,
        None
    );
    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn decimals_are_exact_and_summed_by_the_server() {
    let Some((t, _)) = TestDb::seeded().await else {
        return;
    };
    let db = &t.db;
    let go = Book::objects(db).get(Book::title.eq("Go")).await.unwrap();
    assert_eq!(go.price, Decimal::new(4599, 2));
    let row = Book::objects(db)
        .aggregate([
            ("total", Sum::of(Book::price)),
            ("mean", Avg::of(Book::price)),
        ])
        .await
        .unwrap();
    assert_eq!(
        row.get_as::<Decimal>("total").unwrap(),
        Decimal::new(11848, 2)
    );
    let dear = Book::objects(db)
        .filter(Book::price.gt(Decimal::new(2500, 2)))
        .order_by([Book::price.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&dear), ["Rust", "Go"]);
    t.cleanup().await;
}
