//! Querysets and tokens a cursor page refuses, before any statement runs.

use crate::bare;
use crate::common::{Author, Book, Event, db, seed};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use siderite_orm::{Column, CursorError, Expr, Model, OrmError};
use siderite_orm::{QueryError, QuerySet};

type Page = Result<siderite_orm::CursorPage<Book>, OrmError>;

fn invalid(result: &Result<impl std::fmt::Debug, OrmError>) -> bool {
    matches!(result, Err(OrmError::Query(QueryError::InvalidPlan(_))))
}

fn code(result: Page) -> &'static str {
    match result {
        Err(OrmError::Cursor(e)) => e.code(),
        other => panic!("expected a cursor error, got {other:?}"),
    }
}

#[tokio::test]
async fn querysets_without_a_seek_are_invalid_plans() {
    // No schema: a statement would fail with "no such table".
    let db = bare().await;
    let books = || Book::objects(&db);
    assert!(invalid(&books().cursor_page(0).await));
    assert!(invalid(&books().distinct().cursor_page(2).await));
    assert!(invalid(&books().limit(3).cursor_page(2).await));
    assert!(invalid(&books().offset(3).cursor_page(2).await));
    let grouped = books().annotate("n", siderite_orm::Count::all());
    assert!(invalid(&grouped.cursor_page(2).await));
    let locked = books().select_for_update().cursor_page(2).await;
    assert!(matches!(locked, Err(OrmError::Capability(_))));
}

#[tokio::test]
async fn only_model_columns_can_order_a_page() {
    let db = bare().await;
    let order = |expr: Expr| Book::objects(&db).order_by([expr.asc()]);
    assert!(invalid(&order(Expr::col("nope")).cursor_page(2).await));
    assert!(invalid(
        &order(Expr::col("likes").gt(1)).cursor_page(2).await
    ));
    let joined = Expr::Column(Column::qualified("authors", "name"));
    assert!(invalid(&order(joined).cursor_page(2).await));
    let json = Event::objects(&db).order_by([Expr::col("payload").asc()]);
    assert!(invalid(&json.cursor_page(2).await));
}

#[tokio::test]
async fn after_and_before_cannot_be_combined() {
    let db = bare().await;
    let both = Book::objects(&db).after("x").before("y");
    assert!(invalid(&both.cursor_page(2).await));
    // The same direction twice: the last token is the one checked.
    let twice = Book::objects(&db).after("x").after("@@");
    assert_eq!(code(twice.cursor_page(2).await), "cursor.E001");
}

fn tweak(token: &str, edit: impl FnOnce(&mut Vec<u8>)) -> String {
    let mut raw = URL_SAFE_NO_PAD.decode(token).unwrap();
    edit(&mut raw);
    URL_SAFE_NO_PAD.encode(raw)
}

type Tokens = (QuerySet<Book>, String, String);

async fn first_page(db: &siderite_orm::Db) -> Tokens {
    let qs = Book::objects(db).order_by([Book::likes.desc()]);
    let page = qs.clone().cursor_page(2).await.unwrap();
    let next = page.next.unwrap();
    let later = qs.clone().after(&next).cursor_page(1).await.unwrap();
    (qs, next, later.previous.unwrap())
}

#[tokio::test]
async fn tokens_are_checked_against_the_queryset() {
    let db = db().await;
    seed(&db).await;
    let (qs, next, previous) = first_page(&db).await;
    assert!(qs.clone().after(&next).cursor_page(2).await.is_ok());
    assert!(qs.clone().before(&previous).cursor_page(2).await.is_ok());
    // Malformed: not base64url, oversize, cut short, wrong direction.
    let junk = "@@not base64@@";
    assert_eq!(
        code(qs.clone().after(junk).cursor_page(2).await),
        "cursor.E001"
    );
    let long = format!("{next}{}", "A".repeat(1024));
    assert_eq!(
        code(qs.clone().after(&long).cursor_page(2).await),
        "cursor.E001"
    );
    let short = &next[..next.len() - 4];
    assert_eq!(
        code(qs.clone().after(short).cursor_page(2).await),
        "cursor.E001"
    );
    assert_eq!(
        code(qs.clone().after(&previous).cursor_page(2).await),
        "cursor.E001"
    );
    assert_eq!(
        code(qs.clone().before(&next).cursor_page(2).await),
        "cursor.E001"
    );
    // Another version, another query.
    let version = tweak(&next, |raw| raw[0] = 2);
    assert_eq!(
        code(qs.clone().after(&version).cursor_page(2).await),
        "cursor.E002"
    );
    let foreign = tweak(&next, |raw| raw[3] ^= 1);
    assert_eq!(
        code(qs.clone().after(&foreign).cursor_page(2).await),
        "cursor.E003"
    );
    // Values that do not fit: one too few, text where an integer goes.
    let few = tweak(&next, |raw| raw.truncate(raw.len() - 9));
    assert_eq!(
        code(qs.clone().after(&few).cursor_page(2).await),
        "cursor.E004"
    );
    let text = tweak(&next, |raw| {
        raw.truncate(10);
        raw.extend([2, 0, 0, 0, 0, 2, 0, 0, 0, 0]);
    });
    assert_eq!(
        code(qs.clone().after(&text).cursor_page(2).await),
        "cursor.E004"
    );
}

#[tokio::test]
async fn a_token_only_fits_the_list_it_came_from() {
    let db = db().await;
    seed(&db).await;
    let (qs, next, _) = first_page(&db).await;
    let other_filter = qs.clone().filter(Book::likes.gt(0_i64));
    assert_eq!(
        code(other_filter.after(&next).cursor_page(2).await),
        "cursor.E003"
    );
    let other_value = qs.clone().filter(Book::likes.gt(1_i64));
    let same = qs.clone().filter(Book::likes.gt(1_i64));
    let token = other_value.cursor_page(2).await.unwrap().next.unwrap();
    assert!(same.after(&token).cursor_page(2).await.is_ok());
    let other_value = qs.clone().filter(Book::likes.gt(2_i64));
    assert_eq!(
        code(other_value.after(&token).cursor_page(2).await),
        "cursor.E003"
    );
    let reversed = Book::objects(&db).order_by([Book::likes.asc()]);
    assert_eq!(
        code(reversed.after(&next).cursor_page(2).await),
        "cursor.E003"
    );
    let other_model = Author::objects(&db).order_by([Author::age.desc()]);
    let found = other_model.after(&next).cursor_page(2).await;
    assert!(matches!(
        found,
        Err(OrmError::Cursor(CursorError::ForeignQuery))
    ));
    // Limits and prefetching do not change the list.
    let wider = qs.clone().select_related(Book::author_relation());
    assert!(wider.after(&next).cursor_page(1).await.is_ok());
}

#[tokio::test]
async fn none_never_touches_the_database() {
    let db = bare().await;
    let page = Book::objects(&db).none().cursor_page(3).await.unwrap();
    assert!(page.items.is_empty());
    assert!(page.next.is_none() && page.previous.is_none());
}
