//! Prefetch uses the target compiler's remaining bind capacity.
#![cfg(feature = "sqlite")]

mod common;

use async_trait::async_trait;
use common::TestSqlite;
use common::{Author, Book, db, seed};
use siderite_backends::sql::{Sqlite, compile};
use siderite_orm::{Backend, BackendCapabilities, Db, ExecResult, Executor};
use siderite_orm::{Expr, IsolationLevel, Model, OrmError, Prefetch};
use siderite_orm::{QueryError, QueryPlan, QueryResult, Transaction};
use siderite_orm::{Value, WritePlan};
use std::sync::{Arc, Mutex};

type Scope = Box<dyn Transaction>;
type Args = Vec<Value>;
type Reply<T> = Result<T, OrmError>;
type Fixture = Result<(Db, Bounded), Box<dyn std::error::Error>>;
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Clone)]
struct Bounded {
    inner: TestSqlite,
    max: usize,
    counts: Arc<Mutex<Vec<usize>>>,
    reports: bool,
}

impl Bounded {
    async fn new(max: usize) -> Result<Self, siderite_orm::BackendError> {
        Ok(Self {
            inner: TestSqlite::connect("sqlite::memory:").await?,
            max,
            counts: Arc::default(),
            reports: true,
        })
    }

    fn observed(&self) -> Vec<usize> {
        self.counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[async_trait]
impl Executor for Bounded {
    fn capabilities(&self) -> BackendCapabilities {
        let mut caps = self.inner.capabilities();
        caps.max_params = self.max;
        caps
    }

    async fn fetch(&self, plan: &QueryPlan) -> Reply<QueryResult> {
        let count = compile(plan, &Sqlite)?.params.len();
        if count > self.max {
            let error = QueryError::InvalidPlan("too many binds".into());
            return Err(error.into());
        }
        self.counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(count);
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
impl Backend for Bounded {
    fn read_parameter_count(&self, plan: &QueryPlan) -> Reply<Option<usize>> {
        if self.reports {
            self.inner.read_parameter_count(plan)
        } else {
            Ok(None)
        }
    }

    async fn begin(&self, level: Option<IsolationLevel>) -> Reply<Scope> {
        self.inner.begin(level).await
    }
}

async fn fixture(max: usize) -> Fixture {
    let backing = db().await;
    let backend = Bounded::new(max).await?;
    // Keep schema setup independent of test bind capacity.
    let target = Db::new(backend.inner.clone());
    target
        .execute_script(
            "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT, \
         age INTEGER, team_id INTEGER)",
        )
        .await?;
    seed(&backing).await;
    let rows = backing.raw_sql("SELECT * FROM authors", vec![]).await?;
    let placeholders = vec!["(?, ?, ?, ?)"; rows.rows.len()].join(", ");
    let args = rows
        .rows
        .into_iter()
        .flat_map(|row| row.into_columns().into_iter().map(|(_, v)| v))
        .collect();
    let sql = format!("INSERT INTO authors VALUES {placeholders}");
    target.raw_execute(&sql, args).await?;
    Ok((backing, backend))
}

#[tokio::test]
async fn target_budget_subtracts_existing_filters() -> TestResult {
    let (source, backend) = fixture(3).await?;
    let target = Db::new(backend.clone());
    let targets = Author::objects(&target).filter(Author::id.gt(0));
    let prefetch = Prefetch::new(Book::author_relation()).queryset(targets);
    let books = Book::objects(&source)
        .prefetch_related(prefetch)
        .all()
        .await?;
    assert_eq!(books.len(), 5);
    assert!(books.iter().all(|book| book.author.cached().is_some()));
    assert_eq!(backend.observed(), [3, 2]);
    assert!(std::ptr::eq(
        books[0].author.cached().ok_or("missing author")?,
        books[1].author.cached().ok_or("missing author")?,
    ));
    Ok(())
}

#[tokio::test]
async fn null_literals_consume_no_parameters() -> TestResult {
    let (source, backend) = fixture(1).await?;
    let target = Db::new(backend.clone());
    let filter = Expr::col("id").ne(Expr::Value(Value::Null));
    let base = Author::objects(&target).filter(filter);
    let prefetch = Prefetch::new(Book::author_relation()).queryset(base);
    Book::objects(&source)
        .prefetch_related(prefetch)
        .all()
        .await?;
    assert_eq!(backend.observed(), [1, 1, 1]);
    Ok(())
}

#[tokio::test]
async fn exhausted_and_unknown_budgets_fail_before_target_io() -> TestResult {
    for reports in [false, true] {
        let (source, mut backend) = fixture(1).await?;
        backend.reports = reports;
        let target = Db::new(backend.clone());
        let base = Author::objects(&target).filter(Author::id.gt(0));
        let prefetch = Prefetch::new(Book::author_relation()).queryset(base);
        let result = Book::objects(&source)
            .prefetch_related(prefetch)
            .all()
            .await;
        assert!(matches!(
            result,
            Err(OrmError::Query(QueryError::InvalidPlan(_)))
        ));
        assert!(backend.observed().is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn slices_fail_and_empty_targets_skip_counting() -> TestResult {
    let (source, backend) = fixture(1).await?;
    let target = Db::new(backend.clone());
    let base = Author::objects(&target).limit(1);
    let prefetch = Prefetch::new(Book::author_relation()).queryset(base);
    let result = Book::objects(&source)
        .prefetch_related(prefetch)
        .all()
        .await;
    assert!(matches!(
        result,
        Err(OrmError::Query(QueryError::InvalidPlan(_)))
    ));
    assert!(backend.observed().is_empty());
    let base = Author::objects(&target).none();
    let prefetch = Prefetch::new(Book::author_relation()).queryset(base);
    let books = Book::objects(&source)
        .prefetch_related(prefetch)
        .all()
        .await?;
    assert!(books.iter().all(|book| book.author.cached().is_none()));
    assert!(backend.observed().is_empty());
    Ok(())
}
