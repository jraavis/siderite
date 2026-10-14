//! The cursor pagination contract (`siderite-orm/tests/cursor_contract.json`)
//! run through `QuerySet::cursor_page` on any backend.
//!
//! Two models share the `cursor_items` table: [`Item`] declares its columns
//! nullable (so a page sorts on `IS NULL` keys) and [`Strict`] declares them
//! non-null, which a case without nulls can use. Fixture fields keep their
//! contract names; `group` is stored in the `grp` column.

use serde::Deserialize;
use serde_json::{Map, Value as Json};
use siderite_orm::types::decode;
use siderite_orm::{BackendCapabilityError, Db, DbType, Expr, FieldMeta};
use siderite_orm::{Model, ModelMeta, ModelOps, OrderExpr, OrmError};
use siderite_orm::{QueryError, QuerySet, Row, SqlType, Value, read_column};

const FIXTURES: &str = include_str!(concat!(
    "../../../",
    "siderite-orm/",
    "tests/",
    "cursor_contract.json"
));

/// DDL of the `cursor_items` table for SQLite.
pub const SQLITE_DDL: &str = "CREATE TABLE cursor_items (\
    id INTEGER PRIMARY KEY, score INTEGER, grp TEXT, due INTEGER, name TEXT);";

/// DDL of the `cursor_items` table for PostgreSQL.
pub const PG_DDL: &str = "CREATE TABLE cursor_items (id BIGINT PRIMARY KEY, \
    score BIGINT, grp TEXT, due BIGINT, name TEXT);";

/// DDL of the `cursor_items` table for MySQL.
pub const MYSQL_DDL: &str = "CREATE TABLE cursor_items (\
    id BIGINT PRIMARY KEY, score BIGINT, grp VARCHAR(100), due BIGINT, \
    name VARCHAR(100));";

#[derive(Deserialize)]
struct Fixtures {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    rows: Vec<Map<String, Json>>,
    order: Vec<Term>,
    pages: Vec<PageCase>,
}

#[derive(Deserialize)]
struct Term {
    field: String,
    dir: String,
}

#[derive(Deserialize)]
struct PageCase {
    #[serde(default)]
    before: Option<Writes>,
    limit: u64,
    ids: Vec<i64>,
    more: bool,
}

#[derive(Deserialize)]
struct Writes {
    #[serde(default)]
    insert: Vec<Map<String, Json>>,
    #[serde(default)]
    delete: Vec<i64>,
}

/// A model of the `cursor_items` table that a fixture row converts into.
pub trait Fixture: Model + ModelOps + Send + Sync {
    /// The model for a fixture row (a missing field is null).
    fn from_json(row: &Map<String, Json>) -> Self;
    /// The primary key.
    fn id(&self) -> i64;
}

fn int(row: &Map<String, Json>, key: &str) -> Option<i64> {
    row.get(key).and_then(Json::as_i64)
}

fn text(row: &Map<String, Json>, key: &str) -> Option<String> {
    row.get(key).and_then(Json::as_str).map(str::to_owned)
}

macro_rules! item_model {
    ($name:ident, $meta:ident, $nullable:expr) => {
        /// A row of `cursor_items`.
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            pub id: i64,
            pub score: Option<i64>,
            pub grp: Option<String>,
            pub due: Option<i64>,
            pub name: Option<String>,
        }

        static $meta: ModelMeta = ModelMeta {
            name: stringify!($name),
            table: "cursor_items",
            fields: &[
                FieldMeta {
                    primary_key: true,
                    ..FieldMeta::new("id", "id", SqlType::BigInt)
                },
                FieldMeta {
                    nullable: $nullable,
                    ..FieldMeta::new("score", "score", SqlType::BigInt)
                },
                FieldMeta {
                    nullable: $nullable,
                    ..FieldMeta::new("grp", "grp", SqlType::Text)
                },
                FieldMeta {
                    nullable: $nullable,
                    ..FieldMeta::new("due", "due", SqlType::BigInt)
                },
                FieldMeta {
                    nullable: $nullable,
                    ..FieldMeta::new("name", "name", SqlType::Text)
                },
            ],
            many_to_many: &[],
            ordering: &[],
            indexes: &[],
            constraints: &[],
            managed: true,
        };

        impl Model for $name {
            type Pk = i64;
            const META: &'static ModelMeta = &$meta;
            model_boilerplate!(id: i64, unsaved = |_: &$name| false);
            fn to_values(&self) -> Vec<(&'static str, Value)> {
                vec![
                    ("id", self.id.to_value()),
                    ("score", self.score.to_value()),
                    ("grp", self.grp.to_value()),
                    ("due", self.due.to_value()),
                    ("name", self.name.to_value()),
                ]
            }
            fn from_row(row: &Row, prefix: &str) -> Result<Self, QueryError> {
                Ok(Self {
                    id: read_column(row, prefix, "id")?,
                    score: read_column(row, prefix, "score")?,
                    grp: read_column(row, prefix, "grp")?,
                    due: read_column(row, prefix, "due")?,
                    name: read_column(row, prefix, "name")?,
                })
            }
        }

        impl Fixture for $name {
            fn from_json(row: &Map<String, Json>) -> Self {
                Self {
                    id: int(row, "id").unwrap(),
                    score: int(row, "score"),
                    grp: text(row, "group"),
                    due: int(row, "due"),
                    name: text(row, "name"),
                }
            }
            fn id(&self) -> i64 {
                self.id
            }
        }
    };
}

item_model!(Item, ITEM_META, true);
item_model!(Strict, STRICT_META, false);

fn column(field: &str) -> &str {
    if field == "group" { "grp" } else { field }
}

fn order(terms: &[Term]) -> Vec<OrderExpr> {
    terms
        .iter()
        .map(|t| {
            let expr = Expr::col(column(&t.field).to_owned());
            if t.dir == "desc" {
                expr.desc()
            } else {
                expr.asc()
            }
        })
        .collect()
}

/// Whether any row leaves an ordering field null (or missing).
fn has_nulls(case: &Case) -> bool {
    let inserted = case
        .pages
        .iter()
        .filter_map(|p| p.before.as_ref())
        .flat_map(|w| w.insert.iter());
    case.rows.iter().chain(inserted).any(|row| {
        let missing = |t: &Term| row.get(&t.field).is_none_or(Json::is_null);
        case.order.iter().any(missing)
    })
}

fn ids<M: Fixture>(items: &[M]) -> Vec<i64> {
    items.iter().map(M::id).collect()
}

fn queryset<M: Fixture>(db: &Db, case: &Case) -> QuerySet<M> {
    M::objects(db).order_by(order(&case.order))
}

async fn save<M: Fixture>(db: &Db, row: &Map<String, Json>) {
    let mut item = M::from_json(row);
    item.save(db).await.unwrap();
}

async fn run_case<M: Fixture>(db: &Db, case: &Case) {
    M::objects(db).delete().await.unwrap();
    for row in &case.rows {
        save::<M>(db, row).await;
    }
    let name = format!("{} ({})", case.name, M::META.name);
    let mut token: Option<String> = None;
    let mut forward = Vec::new();
    let mut writes = false;
    for (n, page) in case.pages.iter().enumerate() {
        if let Some(w) = &page.before {
            writes = true;
            for id in &w.delete {
                let gone = M::objects(db).filter(Expr::col("id").eq(*id));
                gone.delete().await.unwrap();
            }
            for row in &w.insert {
                save::<M>(db, row).await;
            }
        }
        let mut qs = queryset::<M>(db, case);
        if let Some(t) = &token {
            qs = qs.after(t);
        }
        let got = qs.cursor_page(page.limit).await.unwrap();
        let label = format!("{name} page {}", n + 1);
        assert_eq!(ids(&got.items), page.ids, "{label}");
        assert_eq!(got.next.is_some(), page.more, "{label}: more");
        assert_eq!(got.previous.is_some(), n > 0, "{label}: previous");
        forward.push((page.ids.clone(), got.previous));
        token = got.next;
    }
    if !writes {
        walk_back::<M>(db, case, &name, &forward).await;
    }
}

/// From the last page, `previous` cursors lead back through every page.
async fn walk_back<M: Fixture>(
    db: &Db,
    case: &Case,
    name: &str,
    forward: &[(Vec<i64>, Option<String>)],
) {
    let limit = case.pages[0].limit;
    let mut at = forward.len() - 1;
    let mut token = forward[at].1.clone();
    while let Some(t) = token {
        at -= 1;
        let got = queryset::<M>(db, case)
            .before(&t)
            .cursor_page(limit)
            .await
            .unwrap();
        assert_eq!(ids(&got.items), forward[at].0, "{name}: back to {at}");
        assert!(got.next.is_some(), "{name}: next after going back");
        assert_eq!(got.previous.is_some(), at > 0, "{name}: previous");
        token = got.previous;
    }
    assert_eq!(at, 0, "{name}: walked back to the first page");
}

/// Whether the case orders by a column that is nullable on [`Item`].
fn order_uses_nullable_column(case: &Case) -> bool {
    case.order.iter().any(|term| {
        let column = column(&term.field);
        Item::META
            .fields
            .iter()
            .any(|field| field.column == column && field.nullable)
    })
}

/// Run every fixture case on `db`, whose `cursor_items` table exists.
///
/// `nullable` says whether the backend can page over nullable columns. If
/// not, a case whose order includes a nullable column must fail with a
/// capability error. An order of only non-null columns, such as the primary
/// key, still runs.
pub async fn run_contract(db: &Db, nullable: bool) {
    let fixtures: Fixtures = serde_json::from_str(FIXTURES).unwrap();
    for case in &fixtures.cases {
        if nullable || !order_uses_nullable_column(case) {
            run_case::<Item>(db, case).await;
        } else {
            let first = queryset::<Item>(db, case).cursor_page(1).await;
            let found = matches!(
                first,
                Err(OrmError::Capability(
                    BackendCapabilityError::Unsupported { .. }
                ))
            );
            assert!(found, "{}: nullable terms need a capability", case.name);
        }
        if !has_nulls(case) {
            run_case::<Strict>(db, case).await;
        }
    }
}
