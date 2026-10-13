//! A backend wrapper that counts statements and can claim the capabilities
//! of another backend, to show the statement budget and that unsupported
//! cursor queries fail before any statement runs.

use crate::common::cursor::{Item, SQLITE_DDL, Strict};
use crate::common::{Book, SCHEMA, seed};
use async_trait::async_trait;
use siderite_backends::sqlite::SqliteBackend;
use siderite_orm::BackendCapabilityError as Unsupported;
use siderite_orm::{Backend, BackendCapabilities, BackendKind};
use siderite_orm::{Db, ExecResult, Executor, Expr, Feature, IsolationLevel};
use siderite_orm::{Model, ModelOps};
use siderite_orm::{OrmError, Prefetch, QueryPlan, QueryResult, Value};
use siderite_orm::{Transaction, WritePlan};
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

type Args = Vec<Value>;
type Reply<T> = Result<T, OrmError>;
type Scope = Box<dyn Transaction>;

#[derive(Clone)]
struct Probe {
    inner: SqliteBackend,
    caps: BackendCapabilities,
    fetches: Arc<AtomicUsize>,
}

impl Probe {
    async fn new(caps: BackendCapabilities) -> Self {
        Self {
            inner: SqliteBackend::connect("sqlite::memory:").await.unwrap(),
            caps,
            fetches: Arc::default(),
        }
    }

    fn fetched(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Executor for Probe {
    fn capabilities(&self) -> BackendCapabilities {
        self.caps.clone()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Reply<QueryResult> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        self.inner.fetch(plan).await
    }

    async fn execute(&self, plan: &WritePlan) -> Reply<ExecResult> {
        self.inner.execute(plan).await
    }

    async fn fetch_raw(&self, sql: &str, args: Args) -> Reply<QueryResult> {
        self.inner.fetch_raw(sql, args).await
    }

    async fn execute_raw(&self, sql: &str, args: Args) -> Reply<u64> {
        self.inner.execute_raw(sql, args).await
    }

    async fn execute_script(&self, sql: &str) -> Reply<()> {
        self.inner.execute_script(sql).await
    }
}

#[async_trait]
impl Backend for Probe {
    fn read_parameter_count(&self, plan: &QueryPlan) -> Reply<Option<usize>> {
        self.inner.read_parameter_count(plan)
    }

    async fn begin(&self, level: Option<IsolationLevel>) -> Reply<Scope> {
        self.inner.begin(level).await
    }
}

fn unsupported(result: &Reply<impl Debug>, kind: BackendKind) -> bool {
    matches!(
        result,
        Err(OrmError::Capability(Unsupported::Unsupported {
            backend,
            feature: Feature::KeysetPagination | Feature::NullableKeyset,
        })) if *backend == kind
    )
}

async fn counted() -> (Db, Probe) {
    let probe = Probe::new(BackendCapabilities::sqlite()).await;
    let db = Db::new(probe.clone());
    db.execute_script(SCHEMA).await.unwrap();
    db.execute_script(SQLITE_DDL).await.unwrap();
    (db, probe)
}

#[tokio::test]
async fn a_page_is_one_statement() {
    let (db, probe) = counted().await;
    for id in 1..=7 {
        let mut item = Item {
            id,
            score: Some(id % 3),
            grp: None,
            due: None,
            name: None,
        };
        item.save(&db).await.unwrap();
    }
    let qs = Item::objects(&db).order_by([Expr::col("score").asc()]);
    let before = probe.fetched();
    let first = qs.clone().cursor_page(3).await.unwrap();
    assert_eq!(probe.fetched() - before, 1);
    let second = qs.clone().after(&first.next.unwrap()).cursor_page(3);
    let second = second.await.unwrap();
    assert_eq!(probe.fetched() - before, 2);
    let back = qs.before(&second.previous.unwrap()).cursor_page(3).await;
    assert_eq!(back.unwrap().items, first.items);
    assert_eq!(probe.fetched() - before, 3);
}

#[tokio::test]
async fn related_objects_cost_no_query_per_row() {
    let (db, probe) = counted().await;
    seed(&db).await;
    let joined = || {
        Book::objects(&db)
            .select_related(Book::author_relation())
            .order_by([Book::likes.desc()])
    };
    let before = probe.fetched();
    let first = joined().cursor_page(3).await.unwrap();
    assert_eq!(probe.fetched() - before, 1);
    assert!(first.items.iter().all(|b| b.author.cached().is_some()));
    let second = joined().after(&first.next.unwrap()).cursor_page(3);
    let second = second.await.unwrap();
    assert_eq!(probe.fetched() - before, 2);
    assert_eq!(second.items.len(), 2);
    assert!(second.items.iter().all(|b| b.author.cached().is_some()));
    // `prefetch_related` adds one statement for the whole page.
    let before = probe.fetched();
    let prefetched = Book::objects(&db)
        .prefetch_related(Prefetch::new(Book::author_relation()))
        .order_by([Book::likes.desc()])
        .cursor_page(4)
        .await
        .unwrap();
    assert_eq!(probe.fetched() - before, 2);
    assert_eq!(prefetched.items.len(), 4);
    assert!(prefetched.items.iter().all(|b| b.author.cached().is_some()));
}

#[tokio::test]
async fn redis_fails_before_any_statement() {
    let probe = Probe::new(BackendCapabilities::redis()).await;
    let db = Db::new(probe.clone());
    let found = Strict::objects(&db).cursor_page(2).await;
    assert!(unsupported(&found, BackendKind::Redis));
    assert_eq!(probe.fetched(), 0);
}

#[tokio::test]
async fn mongodb_pages_non_null_terms_and_rejects_nullable_ones() {
    let probe = Probe::new(BackendCapabilities::mongodb()).await;
    let db = Db::new(probe.clone());
    db.execute_script(SQLITE_DDL).await.unwrap();
    let by_score = Item::objects(&db).order_by([Expr::col("score").asc()]);
    let nullable = by_score.cursor_page(2).await;
    assert!(unsupported(&nullable, BackendKind::MongoDb));
    assert_eq!(probe.fetched(), 0);
    let strict = Strict::objects(&db).cursor_page(2).await.unwrap();
    assert!(strict.items.is_empty());
    assert_eq!(probe.fetched(), 1);
}
