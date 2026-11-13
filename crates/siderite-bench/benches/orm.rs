//! ORM: plan construction, SQL compilation and a 100-row SQLite fetch, with
//! the same fetch through raw SQLx as the driver-only control.
#![allow(clippy::unwrap_used, missing_docs)]

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use siderite::orm::{Db, Row};
use siderite::prelude::*;
use siderite_backends::sql::{Postgres, Sqlite, compile};
use siderite_backends::sqlite::SqliteBackend;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::hint::black_box;

/// Row type for every ORM benchmark.
#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "widgets", ordering = ["id"])]
struct Widget {
    #[field(primary_key, auto)]
    id: i64,
    name: String,
    price: i64,
    active: bool,
}

const SCHEMA: &str = "CREATE TABLE widgets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    price INTEGER NOT NULL,
    active INTEGER NOT NULL
);";

/// [`Widget`] as raw SQLx decodes it, by column position.
#[derive(Debug, sqlx::FromRow)]
struct RawWidget {
    id: i64,
    name: String,
    price: i64,
    active: bool,
}

const INSERT: &str = "INSERT INTO widgets (name, price, active)
    VALUES (?, ?, 1)";
const SELECT: &str = "SELECT id, name, price, active FROM widgets ORDER BY id";

/// A single-connection in-memory pool holding the same `rows` widgets.
async fn seeded_pool(rows: i64) -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::raw_sql(SCHEMA).execute(&pool).await.unwrap();
    for i in 0..rows {
        sqlx::query(INSERT)
            .bind(format!("widget-{i}"))
            .bind(i)
            .execute(&pool)
            .await
            .unwrap();
    }
    pool
}

async fn raw_fetch(pool: &SqlitePool) -> Vec<RawWidget> {
    sqlx::query_as(SELECT).fetch_all(pool).await.unwrap()
}

/// Decode by cloning each value out of its row (`Model::from_row`).
fn borrowed(rows: Vec<Row>) -> Vec<Widget> {
    let decode = |row: &Row| Widget::from_row(row, "").unwrap();
    rows.iter().map(decode).collect()
}

/// Decode by moving each value out of its row (`Model::from_row_mut`).
fn moved(rows: Vec<Row>) -> Vec<Widget> {
    let decode = |mut row: Row| Widget::from_row_mut(&mut row, "").unwrap();
    rows.into_iter().map(decode).collect()
}

fn build(db: &Db) -> QuerySet<Widget> {
    Widget::objects(db)
        .filter(Widget::active.eq(true))
        .filter(Widget::price.gt(10_i64))
        .order_by([Widget::price.desc()])
        .limit(50)
}

async fn seeded_db(rows: i64) -> Db {
    let db = Db::new(SqliteBackend::connect("sqlite::memory:").await.unwrap());
    db.execute_script(SCHEMA).await.unwrap();
    let widgets = (0..rows)
        .map(|i| Widget {
            id: 0,
            name: format!("widget-{i}"),
            price: i,
            active: true,
        })
        .collect();
    Widget::objects(&db).bulk_create(widgets).await.unwrap();
    db
}

fn bench_orm(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let db = runtime.block_on(seeded_db(100));

    let fetched = runtime.block_on(Widget::objects(&db).all()).unwrap();
    assert_eq!(fetched.len(), 100);

    let pool = runtime.block_on(seeded_pool(100));
    let raw = runtime.block_on(raw_fetch(&pool));
    assert_eq!(raw.len(), 100);
    let ids: i64 = raw.iter().map(|row| row.id).sum();
    let prices: i64 = raw.iter().map(|row| row.price).sum();
    assert_eq!((ids, prices), (5050, 4950));
    assert!(raw.iter().all(|row| row.active));
    assert_eq!(raw[99].name, fetched[99].name);

    let mut group = c.benchmark_group("orm");
    group.bench_function("queryset_build", |b| b.iter(|| black_box(build(&db))));

    let plan = build(&db).plan().clone();
    group.bench_function("compile/sqlite", |b| {
        b.iter(|| compile(black_box(&plan), &Sqlite).unwrap());
    });
    group.bench_function("compile/postgres", |b| {
        b.iter(|| compile(black_box(&plan), &Postgres).unwrap());
    });
    group.bench_function("sqlite_fetch_100_rows", |b| {
        b.to_async(&runtime)
            .iter(|| async { Widget::objects(&db).all().await.unwrap() });
    });
    group.bench_function("sqlx_fetch_100_rows", |b| {
        b.to_async(&runtime).iter(|| raw_fetch(&pool));
    });
    let rows = runtime
        .block_on(db.fetch(Widget::objects(&db).plan()))
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 100);
    // Both sides pay for the clone that hands the closure its own rows.
    group.bench_function("decode_100_rows/borrowed", |b| {
        b.iter_batched(|| rows.clone(), borrowed, BatchSize::SmallInput);
    });
    group.bench_function("decode_100_rows/moved", |b| {
        b.iter_batched(|| rows.clone(), moved, BatchSize::SmallInput);
    });
    group.finish();
}

criterion_group!(benches, bench_orm);
criterion_main!(benches);
