//! Cursor (keyset) pagination on in-memory SQLite: the shared contract
//! fixtures, the column types, related-object loading, the statement
//! count, rejected querysets and tokens, and capability errors.
#![cfg(feature = "sqlite")]
#![allow(clippy::unwrap_used)]

#[path = "../common/mod.rs"]
mod common;
mod probe;
mod rejected;
mod types;

use common::cursor::{SQLITE_DDL, run_contract};
use common::{Author, Book, db, seed};
use siderite_backends::sqlite::SqliteBackend;
use siderite_orm::{Db, Model, ModelOps};

async fn bare() -> Db {
    let backend = SqliteBackend::connect("sqlite::memory:").await.unwrap();
    Db::new(backend)
}

#[tokio::test]
async fn the_contract_fixtures_pass() {
    let db = bare().await;
    db.execute_script(SQLITE_DDL).await.unwrap();
    run_contract(&db, true).await;
}

async fn age_pages(db: &Db, descending: bool) -> Vec<Vec<String>> {
    let order = if descending {
        Author::age.desc()
    } else {
        Author::age.asc()
    };
    let qs = Author::objects(db).order_by([order]);
    let mut pages = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let qs = match &token {
            Some(t) => qs.clone().after(t),
            None => qs.clone(),
        };
        let page = qs.cursor_page(1).await.unwrap();
        pages.push(page.items.iter().map(|a| a.name.clone()).collect());
        token = page.next;
        if token.is_none() {
            return pages;
        }
    }
}

#[tokio::test]
async fn nulls_are_last_ascending_and_first_descending() {
    let db = db().await;
    seed(&db).await;
    // Ages: Ann 30, Bob 25, Cy none, Dee 41.
    let asc: Vec<_> = age_pages(&db, false).await.concat();
    assert_eq!(asc, ["Bob", "Ann", "Dee", "Cy"]);
    let desc: Vec<_> = age_pages(&db, true).await.concat();
    assert_eq!(desc, ["Cy", "Dee", "Ann", "Bob"]);
}

#[tokio::test]
async fn an_unordered_queryset_pages_by_primary_key() {
    let db = db().await;
    let s = seed(&db).await;
    let first = Book::objects(&db).cursor_page(2).await.unwrap();
    let ids: Vec<_> = first.items.iter().map(|b| b.id).collect();
    assert_eq!(ids, [s.books[0].id, s.books[1].id]);
    let token = first.next.unwrap();
    let rest = Book::objects(&db)
        .after(&token)
        .cursor_page(10)
        .await
        .unwrap();
    assert_eq!(rest.items.len(), 3);
    assert!(rest.next.is_none() && rest.previous.is_some());
}

#[tokio::test]
async fn pages_before_the_cursor_come_back_in_order() {
    let db = db().await;
    seed(&db).await;
    let qs = Book::objects(&db).order_by([Book::likes.desc()]);
    let all = qs.clone().all().await.unwrap();
    let last = qs.clone().cursor_page(5).await.unwrap();
    assert_eq!(last.items, all);
    assert!(last.next.is_none() && last.previous.is_none());
    // The two rows ahead of the last one, nearest first in ordering.
    let head = qs.clone().cursor_page(3).await.unwrap();
    let tail = qs.clone().after(&head.next.unwrap()).cursor_page(2).await;
    let tail = tail.unwrap();
    assert_eq!(tail.items, all[3..]);
    let back = qs.before(&tail.previous.unwrap()).cursor_page(2).await;
    let back = back.unwrap();
    assert_eq!(back.items, all[1..3]);
    assert!(back.previous.is_some() && back.next.is_some());
}

#[tokio::test]
async fn an_empty_page_keeps_its_place_for_going_back() {
    let db = db().await;
    seed(&db).await;
    let qs = Book::objects(&db).order_by([Book::id.asc()]);
    let head = qs.clone().cursor_page(3).await.unwrap();
    let token = head.next.unwrap();
    // Rows after the cursor vanish between the two requests.
    let gone = Book::objects(&db).filter(Book::id.gt(head.items[2].id));
    assert_eq!(gone.delete().await.unwrap(), 2);
    let empty = qs.clone().after(&token).cursor_page(3).await.unwrap();
    assert!(empty.items.is_empty() && empty.next.is_none());
    // `previous` is the position of the cursor row, so going back lists
    // the rows before it; the row itself ended the page the client left.
    let back = qs.before(&empty.previous.unwrap()).cursor_page(3).await;
    assert_eq!(back.unwrap().items, head.items[..2]);
}

#[tokio::test]
async fn going_back_to_the_start_ends_the_previous_links() {
    let db = db().await;
    seed(&db).await;
    let qs = Book::objects(&db).order_by([Book::id.asc()]);
    let first = qs.clone().cursor_page(1).await.unwrap();
    assert!(first.previous.is_none());
    let second = qs.clone().after(&first.next.unwrap()).cursor_page(1);
    let second = second.await.unwrap();
    let back = qs.before(&second.previous.unwrap()).cursor_page(2);
    let back = back.await.unwrap();
    assert_eq!(back.items, first.items);
    assert!(back.previous.is_none() && back.next.is_some());
    let none = Book::objects(&db).filter(Book::id.lt(0)).cursor_page(2);
    let none = none.await.unwrap();
    assert!(none.items.is_empty());
    assert!(none.next.is_none() && none.previous.is_none());
}

#[tokio::test]
async fn rows_inserted_behind_the_cursor_are_not_seen() {
    let db = db().await;
    let s = seed(&db).await;
    let qs = Book::objects(&db).order_by([Book::title.asc()]);
    let first = qs.clone().cursor_page(2).await.unwrap();
    let books = &first.items;
    let titles: Vec<_> = books.iter().map(|b| b.title.as_str()).collect();
    assert_eq!(titles, ["Async", "Go"]);
    let mut early = Book::new("Aardvark", s.ann.id);
    early.save(&db).await.unwrap();
    let mut late = Book::new("Zzz", s.ann.id);
    late.save(&db).await.unwrap();
    let rest = qs
        .after(&first.next.unwrap())
        .cursor_page(10)
        .await
        .unwrap();
    let titles: Vec<_> = rest.items.iter().map(|b| b.title.as_str()).collect();
    assert_eq!(titles, ["Rust", "SQL", "Zig", "Zzz"]);
}
