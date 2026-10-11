use crate::common::Event;
use crate::fixtures::TestDb;
use chrono::TimeZone as _;
use chrono::{NaiveDate, NaiveTime, Utc};
use siderite_orm::Expr;
use siderite_orm::{Model as _, ModelOps as _};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires explicit MONGODB_URL and an isolated live service"]
async fn date_parts_functions_casts_and_case() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let mut e = Event {
        id: Uuid::new_v4(),
        at: Utc.with_ymd_and_hms(2026, 9, 29, 8, 5, 3).unwrap(),
        day: NaiveDate::from_ymd_opt(2026, 9, 29).unwrap(),
        clock: NaiveTime::from_hms_micro_opt(13, 45, 6, 250_000).unwrap(),
        payload: serde_json::json!({}),
        blob: vec![],
        score: Some(2.5),
    };
    e.save(db).await.unwrap();
    let row = Event::objects(db)
        .annotate("y", Event::day.year())
        .annotate("m", Event::day.month())
        .annotate("d", Event::day.day())
        .annotate("w", Event::day.week())
        .annotate("q", Event::day.quarter())
        .annotate("day_of", Event::at.date())
        .annotate("ah", Event::at.hour())
        .annotate("am", Event::at.minute())
        .annotate("as", Event::at.second())
        .annotate("h", Event::clock.hour())
        .annotate("mi", Event::clock.minute())
        .annotate("s", Event::clock.second())
        .annotate("trimmed", siderite_orm::functions::trim(Expr::val("  x ")))
        .annotate(
            "part",
            siderite_orm::functions::substr(Expr::val("abcdef"), 2, Some(3)),
        )
        .annotate(
            "rest",
            siderite_orm::functions::substr(Expr::val("abcdef"), 4, None),
        )
        .annotate(
            "swapped",
            siderite_orm::functions::replace(Expr::val("a-b-c"), "-", "+"),
        )
        .annotate("as_text", Event::score.cast(siderite_orm::SqlType::Text))
        .annotate(
            "as_int",
            Expr::val("42").cast(siderite_orm::SqlType::BigInt),
        )
        .annotate(
            "size",
            siderite_orm::functions::case()
                .when(Event::score.gt(2.0), Expr::val("big"))
                .otherwise(Expr::val("small")),
        )
        .project([
            "y", "m", "d", "w", "q", "day_of", "ah", "am", "as", "h", "mi", "s", "trimmed", "part",
            "rest", "swapped", "as_text", "as_int", "size",
        ])
        .rows()
        .await
        .unwrap()
        .remove(0);
    let int = |c: &str| row.get_as::<i64>(c).unwrap();
    assert_eq!(
        (int("y"), int("m"), int("d"), int("w"), int("q")),
        (2026, 9, 29, 40, 3)
    );
    assert_eq!(row.get_as::<String>("day_of").unwrap(), "2026-09-29");
    assert_eq!((int("ah"), int("am"), int("as")), (8, 5, 3));
    assert_eq!((int("h"), int("mi"), int("s")), (13, 45, 6));
    assert_eq!(row.get_as::<String>("trimmed").unwrap(), "x");
    assert_eq!(row.get_as::<String>("part").unwrap(), "bcd");
    assert_eq!(row.get_as::<String>("rest").unwrap(), "def");
    assert_eq!(row.get_as::<String>("swapped").unwrap(), "a+b+c");
    assert_eq!(row.get_as::<String>("as_text").unwrap(), "2.5");
    assert_eq!(int("as_int"), 42);
    assert_eq!(row.get_as::<String>("size").unwrap(), "big");
    // Filtering on a computed part.
    assert_eq!(
        Event::objects(db)
            .filter(Event::clock.hour().eq(13_i64))
            .count()
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        Event::objects(db)
            .filter(Event::at.minute().eq(6_i64))
            .count()
            .await
            .unwrap(),
        0
    );
    t.cleanup().await;
}
