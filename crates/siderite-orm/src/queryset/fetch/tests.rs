//! The plan `get` sends: which ordering survives the `LIMIT 2` probe.
//! Row decoding and real SQL run against SQLite in `siderite-backends`.

use crate::Count;
use crate::backend::{Backend, ExecResult, Executor, QueryResult, Row, Transaction};
use crate::capabilities::{BackendCapabilities, IsolationLevel};
use crate::db::Db;
use crate::error::{OrmError, QueryError};
use crate::expr::Expr;
use crate::model::{FieldMeta, Model, ModelMeta};
use crate::plan::{DistinctMode, LockMode, OrderDirection, OrderExpr, QueryPlan};
use crate::types::{SqlType, decode};
use crate::value::Value;
use crate::write::WritePlan;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

#[derive(Debug, PartialEq)]
struct Post {
    id: i64,
}

static POST_FIELDS: [FieldMeta; 2] = [
    FieldMeta {
        primary_key: true,
        ..FieldMeta::new("id", "id", SqlType::BigInt)
    },
    FieldMeta::new("title", "title", SqlType::Text),
];

static POST_META: ModelMeta = ModelMeta {
    name: "Post",
    table: "posts",
    fields: &POST_FIELDS,
    many_to_many: &[],
    ordering: &[("id", OrderDirection::Desc)],
    indexes: &[],
    constraints: &[],
    managed: true,
};

impl Model for Post {
    type Pk = i64;
    const META: &'static ModelMeta = &POST_META;

    fn pk(&self) -> i64 {
        self.id
    }

    fn set_pk(&mut self, value: Value) -> Result<(), QueryError> {
        self.id = decode("id", value)?;
        Ok(())
    }

    fn is_unsaved(&self) -> bool {
        false
    }

    fn to_values(&self) -> Vec<(&'static str, Value)> {
        vec![("id", Value::Int(self.id))]
    }

    fn from_row(row: &Row, _: &str) -> Result<Self, QueryError> {
        let id = row.get("id").cloned().unwrap_or(Value::Null);
        Ok(Self {
            id: decode("id", id)?,
        })
    }
}

/// Records every fetched plan and answers with `rows` posts.
#[derive(Clone)]
struct Recorder {
    plans: Arc<Mutex<Vec<QueryPlan>>>,
    rows: i64,
}

#[async_trait]
impl Executor for Recorder {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::sqlite()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        self.plans
            .lock()
            .map_err(|_| QueryError::TransactionAborted)?
            .push(plan.clone());
        let rows = (1..=self.rows)
            .map(|id| Row::new(vec![("id".into(), Value::Int(id))]))
            .collect();
        Ok(QueryResult { rows })
    }

    async fn execute(&self, _: &WritePlan) -> Result<ExecResult, OrmError> {
        Ok(ExecResult::default())
    }

    async fn fetch_raw(&self, _: &str, _: Vec<Value>) -> Result<QueryResult, OrmError> {
        Ok(QueryResult::default())
    }

    async fn execute_raw(&self, _: &str, _: Vec<Value>) -> Result<u64, OrmError> {
        Ok(0)
    }

    async fn execute_script(&self, _: &str) -> Result<(), OrmError> {
        Ok(())
    }
}

#[async_trait]
impl Backend for Recorder {
    async fn begin(&self, _: Option<IsolationLevel>) -> Result<Box<dyn Transaction>, OrmError> {
        Ok(Box::new(self.clone()))
    }
}

#[async_trait]
impl Transaction for Recorder {
    async fn commit(&self) -> Result<(), OrmError> {
        Ok(())
    }

    async fn rollback(&self) -> Result<(), OrmError> {
        Ok(())
    }
}

/// A database answering every query with `rows` posts.
fn recorded(rows: i64) -> (Db, Recorder) {
    let recorder = Recorder {
        plans: Arc::default(),
        rows,
    };
    (Db::new(recorder.clone()), recorder)
}

/// The one plan the queryset fetched.
fn sent(recorder: &Recorder) -> QueryPlan {
    let plans = recorder.plans.lock().map(|p| p.clone()).unwrap_or_default();
    assert_eq!(plans.len(), 1, "one query per get: {plans:?}");
    plans
        .into_iter()
        .next()
        .unwrap_or_else(|| QueryPlan::from_table(""))
}

fn by_id(direction: OrderDirection) -> Vec<OrderExpr> {
    vec![OrderExpr {
        expr: Expr::col("id"),
        direction,
    }]
}

fn wanted() -> Expr {
    Expr::col("id").eq(1)
}

#[tokio::test]
async fn get_drops_the_default_ordering() -> Result<(), OrmError> {
    let (db, recorder) = recorded(1);
    let posts = Post::objects(&db);
    assert_eq!(posts.plan().ordering, by_id(OrderDirection::Desc));
    assert_eq!(posts.get(wanted()).await?, Post { id: 1 });
    let plan = sent(&recorder);
    assert!(plan.ordering.is_empty(), "{plan:?}");
    assert_eq!(plan.limit, Some(2));
    assert_eq!(plan.filter, Some(wanted()));
    Ok(())
}

#[tokio::test]
async fn get_drops_an_explicit_ordering() -> Result<(), OrmError> {
    let (db, recorder) = recorded(1);
    Post::objects(&db)
        .order_by([Expr::col("title").asc()])
        .get(wanted())
        .await?;
    let plan = sent(&recorder);
    assert!(plan.ordering.is_empty(), "{plan:?}");
    assert_eq!(plan.limit, Some(2));
    Ok(())
}

#[tokio::test]
async fn a_sliced_get_keeps_its_ordering() -> Result<(), OrmError> {
    let (db, recorder) = recorded(1);
    Post::objects(&db).offset(3).get(wanted()).await?;
    let plan = sent(&recorder);
    assert_eq!(plan.ordering, by_id(OrderDirection::Desc));
    assert_eq!(plan.offset, Some(3));

    let (db, recorder) = recorded(1);
    let ordering = vec![Expr::col("title").asc()];
    Post::objects(&db)
        .order_by(ordering.clone())
        .limit(10)
        .get(wanted())
        .await?;
    assert_eq!(sent(&recorder).ordering, ordering);
    Ok(())
}

#[tokio::test]
async fn get_never_widens_a_narrower_slice() -> Result<(), OrmError> {
    let (db, recorder) = recorded(1);
    Post::objects(&db).limit(1).get(wanted()).await?;
    assert_eq!(sent(&recorder).limit, Some(1));

    let (db, recorder) = recorded(1);
    Post::objects(&db).limit(10).get(wanted()).await?;
    assert_eq!(sent(&recorder).limit, Some(2));
    Ok(())
}

#[tokio::test]
async fn distinct_on_keeps_the_ordering_that_picks_each_row() -> Result<(), OrmError> {
    let (db, recorder) = recorded(1);
    let ordering = vec![Expr::col("title").asc(), Expr::col("id").desc()];
    Post::objects(&db)
        .distinct_on([Expr::col("title")])
        .order_by(ordering.clone())
        .get(wanted())
        .await?;
    let plan = sent(&recorder);
    assert_eq!(plan.ordering, ordering);
    assert_eq!(plan.distinct, DistinctMode::On(vec![Expr::col("title")]));

    // Plain DISTINCT returns the same set in any order.
    let (db, recorder) = recorded(1);
    Post::objects(&db).distinct().get(wanted()).await?;
    let plan = sent(&recorder);
    assert!(plan.ordering.is_empty(), "{plan:?}");
    assert_eq!(plan.distinct, DistinctMode::All);
    Ok(())
}

#[tokio::test]
async fn dropping_an_aggregate_ordering_keeps_its_grouping() -> Result<(), OrmError> {
    let (db, recorder) = recorded(1);
    let posts = Post::objects(&db).order_by([Expr::from(Count::all()).desc()]);
    let grouping = posts.plan().grouping.clone();
    assert!(!grouping.is_empty());
    posts.get(wanted()).await?;
    let plan = sent(&recorder);
    assert!(plan.ordering.is_empty(), "{plan:?}");
    assert_eq!(plan.grouping, grouping, "same rows as the ordered query");
    Ok(())
}

#[tokio::test]
async fn a_locked_get_keeps_the_lock_but_not_the_ordering() -> Result<(), OrmError> {
    let (db, recorder) = recorded(1);
    Post::objects(&db).select_for_update().get(wanted()).await?;
    let plan = sent(&recorder);
    assert!(plan.ordering.is_empty(), "{plan:?}");
    assert_eq!(plan.lock, Some(LockMode::ForUpdate));
    Ok(())
}

#[tokio::test]
async fn get_still_reports_missing_and_multiple() -> Result<(), OrmError> {
    let (db, recorder) = recorded(0);
    assert!(matches!(
        Post::objects(&db).get(wanted()).await,
        Err(OrmError::Query(QueryError::DoesNotExist))
    ));
    assert!(sent(&recorder).ordering.is_empty());

    let (db, recorder) = recorded(2);
    assert!(matches!(
        Post::objects(&db).get(wanted()).await,
        Err(OrmError::Query(QueryError::MultipleObjectsReturned(2)))
    ));
    assert!(sent(&recorder).ordering.is_empty());
    Ok(())
}
