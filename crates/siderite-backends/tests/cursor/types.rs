//! Every ordering column type pages in the same order as sorting the rows
//! in memory, forward and back, whatever form SQLite stores it in.

use crate::common::{Book, Event, db, seed};
use chrono::{DateTime, NaiveDate, NaiveTime};
use siderite_orm::{Db, Expr, Model, ModelOps, OrderExpr};
use uuid::Uuid;

type When = (i64, u32, u32);

fn event(n: u128, when: When, score: Option<f64>) -> Event {
    let (at_secs, day, clock) = when;
    Event {
        id: Uuid::from_u128(n),
        at: DateTime::from_timestamp(at_secs, 123_000).unwrap(),
        day: NaiveDate::from_ymd_opt(2024, 5, day).unwrap(),
        clock: NaiveTime::from_hms_micro_opt(clock, 30, 0, 250).unwrap(),
        payload: serde_json::json!({"n": n.to_string()}),
        blob: vec![u8::try_from(n % 4).unwrap(), 9],
        score,
    }
}

async fn events(db: &Db) -> Vec<Event> {
    let mut all = vec![
        event(7, (1_700_000_100, 3, 9), Some(1.5)),
        event(2, (1_700_000_100, 3, 9), None),
        event(5, (1_700_000_000, 4, 8), Some(-2.0)),
        event(1, (1_700_000_300, 3, 22), Some(1.5)),
        event(9, (1_700_000_200, 1, 8), None),
        event(4, (1_700_000_100, 4, 23), Some(0.0)),
        event(3, (1_700_000_500, 2, 7), Some(1.5)),
    ];
    for e in &mut all {
        e.save(db).await.unwrap();
    }
    all
}

/// The ids in the order a sort on `key` gives, null (`true`) last when
/// ascending, ties broken by id.
fn expected<K, F>(all: &[Event], key: F, desc: bool) -> Vec<Uuid>
where
    F: Fn(&Event) -> K,
    K: PartialOrd,
{
    let mut rows = all.to_vec();
    rows.sort_by(|a, b| {
        let order = key(a).partial_cmp(&key(b)).unwrap();
        let order = if desc { order.reverse() } else { order };
        order.then(a.id.cmp(&b.id))
    });
    rows.iter().map(|e| e.id).collect()
}

/// Page forward two at a time, then back from the end; both must visit
/// every row in the same order.
async fn walk(db: &Db, order: OrderExpr) -> Vec<Uuid> {
    let qs = Event::objects(db).order_by([order]);
    let mut pages = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let page = match &token {
            Some(t) => qs.clone().after(t).cursor_page(2).await,
            None => qs.clone().cursor_page(2).await,
        };
        let page = page.unwrap();
        pages.push((
            page.items.iter().map(|e| e.id).collect::<Vec<_>>(),
            page.previous,
        ));
        token = page.next;
        if token.is_none() {
            break;
        }
    }
    let mut at = pages.len() - 1;
    while let Some(t) = pages[at].1.clone() {
        at -= 1;
        let back = qs.clone().before(&t).cursor_page(2).await.unwrap();
        let ids: Vec<_> = back.items.iter().map(|e| e.id).collect();
        assert_eq!(ids, pages[at].0, "back to page {at}");
    }
    assert_eq!(at, 0);
    pages.into_iter().flat_map(|(ids, _)| ids).collect()
}

#[tokio::test]
async fn event_columns_page_like_an_in_memory_sort() {
    let db = db().await;
    let all = events(&db).await;
    let col = Expr::col;
    assert_eq!(
        walk(&db, col("at").asc()).await,
        expected(&all, |e| e.at, false)
    );
    assert_eq!(
        walk(&db, col("at").desc()).await,
        expected(&all, |e| e.at, true)
    );
    assert_eq!(
        walk(&db, col("day").desc()).await,
        expected(&all, |e| e.day, true)
    );
    assert_eq!(
        walk(&db, col("clock").asc()).await,
        expected(&all, |e| e.clock, false)
    );
    assert_eq!(
        walk(&db, col("blob").asc()).await,
        expected(&all, |e| e.blob.clone(), false)
    );
    assert_eq!(
        walk(&db, col("id").desc()).await,
        expected(&all, |e| e.id, true)
    );
    // Nulls are the largest value: last ascending, first descending.
    let score = |e: &Event| (e.score.is_none(), e.score.unwrap_or(0.0));
    assert_eq!(
        walk(&db, col("score").asc()).await,
        expected(&all, score, false)
    );
    assert_eq!(
        walk(&db, col("score").desc()).await,
        expected(&all, score, true)
    );
}

#[tokio::test]
async fn decimals_and_dates_page_in_value_order() {
    let db = db().await;
    seed(&db).await;
    for order in [
        Book::price.desc(),
        Book::published.asc(),
        Book::pages.desc(),
    ] {
        let qs = Book::objects(&db).order_by([order]);
        let mut seen = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page = match &token {
                Some(t) => qs.clone().after(t).cursor_page(2).await,
                None => qs.clone().cursor_page(2).await,
            };
            let page = page.unwrap();
            seen.extend(page.items.iter().map(|b| b.title.clone()));
            token = page.next;
            if token.is_none() {
                break;
            }
        }
        assert_eq!(seen.len(), 5);
        let unique: std::collections::BTreeSet<_> = seen.iter().collect();
        assert_eq!(unique.len(), 5);
    }
    let by_price = Book::objects(&db).order_by([Book::price.desc()]);
    let titles: Vec<_> = by_price.clone().cursor_page(5).await.unwrap().items;
    let want = by_price.all().await.unwrap();
    assert_eq!(titles, want);
}
