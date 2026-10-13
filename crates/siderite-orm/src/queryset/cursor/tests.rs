//! Token codec, ordering resolution, fingerprint and predicate tests.
//! The pages themselves run against SQLite in `siderite-backends`.

#![allow(clippy::unwrap_used)]

use super::Way;
use super::seek::{self, Seek};
use super::token::{self, Slot};
use crate::backend::Row;
use crate::error::{CursorError, QueryError};
use crate::expr::Expr;
use crate::model::{FieldMeta, Model, ModelMeta};
use crate::plan::{OrderDirection, QueryPlan};
use crate::types::SqlType;
use crate::value::Value;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, NaiveDate, NaiveTime};
use rust_decimal::Decimal;
use uuid::Uuid;

const PRINT: u64 = 0x5eed_cafe_f00d_0001;

fn slot(sql_type: SqlType, nullable: bool) -> Slot {
    Slot { sql_type, nullable }
}

fn ints() -> [Slot; 2] {
    [slot(SqlType::BigInt, false), slot(SqlType::BigInt, false)]
}

fn bytes(token: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD.decode(token).unwrap()
}

fn token_of(raw: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(raw)
}

/// The token for `values` as the first page's `next`.
fn made(values: &[Value]) -> String {
    token::encode(PRINT, Way::After, values).unwrap()
}

/// Decode `token` against two non-null integer terms.
fn read(token: &str) -> Result<Vec<Value>, CursorError> {
    token::decode(token, PRINT, Way::After, &ints())
}

fn code(result: Result<Vec<Value>, CursorError>) -> &'static str {
    result.unwrap_err().code()
}

#[test]
fn every_value_type_round_trips_in_its_own_tag() {
    let cases = [
        (SqlType::BigInt, Value::Int(-7)),
        (SqlType::Duration, Value::Int(i64::MAX)),
        (SqlType::Text, Value::Text("héllo".into())),
        (SqlType::Double, Value::Float(-0.5)),
        (SqlType::Bool, Value::Bool(true)),
        (SqlType::Binary, Value::Bytes(vec![0, 255, 7])),
        (
            SqlType::Timestamp,
            Value::Timestamp(DateTime::from_timestamp_micros(-5).unwrap()),
        ),
        (
            SqlType::Date,
            Value::Date(NaiveDate::from_ymd_opt(1969, 7, 20).unwrap()),
        ),
        (SqlType::Uuid, Value::Uuid(Uuid::from_u128(9))),
        (SqlType::Decimal, Value::Decimal(Decimal::new(-1250, 2))),
        (
            SqlType::Time,
            Value::Time(NaiveTime::from_hms_micro_opt(1, 2, 3, 4).unwrap()),
        ),
    ];
    for (ty, value) in cases {
        let slots = [slot(ty, true), slot(SqlType::BigInt, false)];
        let values = [value, Value::Int(1)];
        let made = token::encode(PRINT, Way::After, &values).unwrap();
        let back = token::decode(&made, PRINT, Way::After, &slots).unwrap();
        assert_eq!(back, values, "{ty:?}");
    }
}

#[test]
fn tags_zero_to_two_match_the_contract_layout() {
    let values = [Value::Null, Value::Int(20), Value::Text("ab".into())];
    let made = token::encode(PRINT, Way::After, &values).unwrap();
    let mut want = vec![1];
    want.extend(PRINT.to_be_bytes());
    want.extend([b'a', 0, 1]);
    want.extend(20_i64.to_be_bytes());
    want.extend([2, 0, 0, 0, 2, b'a', b'b']);
    assert_eq!(bytes(&made), want);
    let before = token::encode(PRINT, Way::Before, &[]).unwrap();
    assert_eq!(bytes(&before)[9], b'b');
}

#[test]
fn new_tags_have_a_fixed_layout() {
    let at = |v: Value| bytes(&made(&[v]));
    let tail = |v: Value| at(v)[10..].to_vec();
    assert_eq!(tail(Value::Float(1.0))[0], 3);
    assert_eq!(tail(Value::Bool(true)), [4, 1]);
    assert_eq!(tail(Value::Bytes(vec![9])), [5, 0, 0, 0, 1, 9]);
    let stamp = DateTime::from_timestamp_micros(258).unwrap();
    let mut want = vec![6];
    want.extend(258_i64.to_be_bytes());
    assert_eq!(tail(Value::Timestamp(stamp)), want);
    let day = NaiveDate::from_ymd_opt(1970, 1, 3).unwrap();
    assert_eq!(tail(Value::Date(day)), [7, 0, 0, 0, 2]);
    let uuid = Uuid::from_u128(1);
    assert_eq!(tail(Value::Uuid(uuid))[..2], [8, 0]);
    let price = Decimal::new(125, 1);
    let mut want = vec![9, 0, 0, 0, 4];
    want.extend(b"12.5");
    assert_eq!(tail(Value::Decimal(price)), want);
    let noon = NaiveTime::from_hms_opt(0, 0, 1).unwrap();
    let mut want = vec![10];
    want.extend(1_000_000_i64.to_be_bytes());
    assert_eq!(tail(Value::Time(noon)), want);
}

#[test]
fn each_defect_has_its_code() {
    let ok = made(&[Value::Int(20), Value::Int(5)]);
    let decode = read;
    assert_eq!(decode(&ok).unwrap(), [Value::Int(20), Value::Int(5)]);
    assert_eq!(code(decode("@@not base64@@")), "cursor.E001");
    assert_eq!(
        code(decode(&format!("{ok}{}", "A".repeat(1024)))),
        "cursor.E001"
    );
    assert_eq!(code(decode(&ok[..ok.len() - 4])), "cursor.E001");
    assert_eq!(code(decode("")), "cursor.E001");
    let mut raw = bytes(&ok);
    raw[0] = 2;
    assert_eq!(code(decode(&token_of(&raw))), "cursor.E002");
    raw[0] = 1;
    raw[8] ^= 1;
    assert_eq!(code(decode(&token_of(&raw))), "cursor.E003");
    raw[8] ^= 1;
    raw[9] = b'b';
    assert_eq!(code(decode(&token_of(&raw))), "cursor.E001");
    let wrong = token::decode(&ok, PRINT, Way::Before, &ints());
    assert_eq!(code(wrong), "cursor.E001");
}

#[test]
fn value_defects_are_malformed_and_misfits_are_mismatches() {
    let header = |way: u8| {
        let mut raw = vec![1];
        raw.extend(PRINT.to_be_bytes());
        raw.push(way);
        raw
    };
    let decode = |raw: Vec<u8>| read(&token_of(&raw));
    let tagged = |tail: &[u8]| {
        let mut raw = header(b'a');
        raw.extend(tail);
        raw
    };
    assert_eq!(code(decode(tagged(&[99]))), "cursor.E001");
    assert_eq!(
        code(decode(tagged(&[2, 0, 0, 0, 2, 0xff, 0xfe]))),
        "cursor.E001"
    );
    assert_eq!(
        code(decode(tagged(&[2, 0xff, 0xff, 0xff, 0xff]))),
        "cursor.E001"
    );
    assert_eq!(code(decode(tagged(&[4, 2]))), "cursor.E001");
    assert_eq!(code(decode(tagged(&[9, 0, 0, 0, 1, b'x']))), "cursor.E001");
    assert_eq!(
        code(decode(tagged(&[
            10, 255, 255, 255, 255, 255, 255, 255, 255
        ]))),
        "cursor.E001"
    );
    assert_eq!(code(decode(tagged(&[1, 0]))), "cursor.E001");
    // Too few values, a null in a non-null term, text in an integer term.
    let one = 1_i64.to_be_bytes();
    let int = |raw: &mut Vec<u8>| {
        raw.push(1);
        raw.extend(one);
    };
    let mut few = header(b'a');
    int(&mut few);
    assert_eq!(code(decode(few)), "cursor.E004");
    let mut null = header(b'a');
    null.push(0);
    int(&mut null);
    assert_eq!(code(decode(null)), "cursor.E004");
    let text = tagged(&[2, 0, 0, 0, 0]);
    let mut text_first = text.clone();
    int(&mut text_first);
    assert_eq!(code(decode(text_first)), "cursor.E004");
}

#[test]
fn a_null_fits_a_nullable_term() {
    let slots = [slot(SqlType::BigInt, true), slot(SqlType::BigInt, false)];
    let values = [Value::Null, Value::Int(1)];
    let made = token::encode(PRINT, Way::After, &values).unwrap();
    let back = token::decode(&made, PRINT, Way::After, &slots).unwrap();
    assert_eq!(back, values);
}

#[test]
fn a_position_too_large_for_a_token_is_not_issued() {
    let big = Value::Text("x".repeat(2000));
    let made = token::encode(PRINT, Way::After, &[big]);
    assert!(matches!(made, Err(QueryError::InvalidPlan(_))));
    let json = Value::Json(serde_json::json!(1));
    assert!(token::encode(PRINT, Way::After, &[json]).is_err());
}

#[test]
fn a_time_finer_than_a_microsecond_is_not_truncated() {
    let at = DateTime::from_timestamp(1_700_000_000, 1_500).unwrap();
    let made = token::encode(PRINT, Way::After, &[Value::Timestamp(at)]);
    assert!(matches!(made, Err(QueryError::InvalidPlan(_))));
    let time = NaiveTime::from_hms_nano_opt(1, 2, 3, 7).unwrap();
    let made = token::encode(PRINT, Way::After, &[Value::Time(time)]);
    assert!(matches!(made, Err(QueryError::InvalidPlan(_))));
    let exact = DateTime::from_timestamp(1_700_000_000, 2_000).unwrap();
    let values = [Value::Timestamp(exact)];
    let slots = [slot(SqlType::Timestamp, false)];
    let made = token::encode(PRINT, Way::After, &values).unwrap();
    let back = token::decode(&made, PRINT, Way::After, &slots).unwrap();
    assert_eq!(back, values);
}

#[test]
fn canonical_values_do_not_depend_on_the_backend_form() {
    use SqlType as T;
    let text = |s: &str| Value::Text(s.into());
    let at = DateTime::from_timestamp_micros(1_700_000_000_123_456).unwrap();
    let cases = [
        (T::Bool, Value::Int(1), Value::Bool(true)),
        (
            T::Timestamp,
            text("2023-11-14T22:13:20.123456Z"),
            Value::Timestamp(at),
        ),
        (
            T::Uuid,
            text("00000000-0000-0000-0000-000000000009"),
            Value::Uuid(Uuid::from_u128(9)),
        ),
        (
            T::Decimal,
            text("12.50"),
            Value::Decimal(Decimal::new(1250, 2)),
        ),
        (T::Double, Value::Int(2), Value::Float(2.0)),
        (T::IpAddr, text("::1"), text("::1")),
    ];
    for (ty, raw, want) in cases {
        assert_eq!(seek::canonical("c", ty, raw).unwrap(), want, "{ty:?}");
    }
    assert!(
        seek::canonical("c", T::BigInt, Value::Null)
            .unwrap()
            .is_null()
    );
    let bad = seek::canonical("c", T::BigInt, text("x"));
    assert!(matches!(bad, Err(QueryError::Decode { .. })));
    assert!(seek::canonical("c", T::Json, text("{}")).is_err());
}

// ---------------------------------------------------------------------
// Ordering, fingerprint and predicate
// ---------------------------------------------------------------------

struct Item;

static ITEM_FIELDS: [FieldMeta; 4] = [
    FieldMeta {
        primary_key: true,
        ..FieldMeta::new("id", "id", SqlType::BigInt)
    },
    FieldMeta {
        nullable: true,
        ..FieldMeta::new("due", "due", SqlType::BigInt)
    },
    FieldMeta::new("name", "name", SqlType::Text),
    FieldMeta::new("data", "data", SqlType::Json),
];

static ITEM_META: ModelMeta = ModelMeta {
    name: "Item",
    table: "items",
    fields: &ITEM_FIELDS,
    many_to_many: &[],
    ordering: &[],
    indexes: &[],
    constraints: &[],
    managed: true,
};

impl Model for Item {
    type Pk = i64;
    const META: &'static ModelMeta = &ITEM_META;

    fn pk(&self) -> i64 {
        0
    }

    fn set_pk(&mut self, _: Value) -> Result<(), QueryError> {
        Ok(())
    }

    fn is_unsaved(&self) -> bool {
        false
    }

    fn to_values(&self) -> Vec<(&'static str, Value)> {
        Vec::new()
    }

    fn from_row(_: &Row, _: &str) -> Result<Self, QueryError> {
        Err(QueryError::Model("not decoded in these tests".into()))
    }
}

fn plan() -> QueryPlan {
    QueryPlan::from_table("items")
}

fn columns(plan: &QueryPlan) -> Vec<(&'static str, OrderDirection)> {
    let terms = seek::terms::<Item>(plan).unwrap();
    terms
        .iter()
        .map(|t| (t.field.column, t.direction))
        .collect()
}

#[test]
fn the_primary_key_is_appended_unless_present() {
    use OrderDirection::{Asc, Desc};
    assert_eq!(columns(&plan()), [("id", Asc)]);
    let by_due = plan().order_by(Expr::col("due"), Desc);
    assert_eq!(columns(&by_due), [("due", Desc), ("id", Asc)]);
    let by_id = plan().order_by(Expr::col("id"), Desc);
    assert_eq!(columns(&by_id), [("id", Desc)]);
}

#[test]
fn only_model_columns_can_order_a_cursor_page() {
    let asc = OrderDirection::Asc;
    let outside = crate::expr::Column::qualified("a", "id");
    let rejected = [
        plan().order_by(Expr::col("nope"), asc),
        plan().order_by(Expr::col("data"), asc),
        plan().order_by(Expr::col("id").gt(1), asc),
        plan().order_by(Expr::Column(outside), asc),
    ];
    for plan in rejected {
        let found = seek::terms::<Item>(&plan);
        assert!(matches!(found, Err(QueryError::InvalidPlan(_))));
    }
    let qualified = crate::expr::Column::qualified("items", "id");
    let own = plan().order_by(Expr::Column(qualified), asc);
    assert!(seek::terms::<Item>(&own).is_ok());
}

#[test]
fn querysets_a_seek_cannot_express_are_rejected() {
    use crate::plan::{DistinctMode, LockMode};
    assert!(seek::check_shape(&plan()).is_ok());
    let bad = [
        plan().distinct(DistinctMode::All),
        plan().limit(3),
        plan().offset(3),
        plan().lock(LockMode::ForUpdate),
    ];
    for plan in bad {
        assert!(seek::check_shape(&plan).is_err());
    }
    let mut grouped = plan();
    grouped.grouping.push(Expr::col("name"));
    assert!(seek::check_shape(&grouped).is_err());
    let mut having = plan();
    having.having = Some(Expr::col("id").gt(1));
    assert!(seek::check_shape(&having).is_err());
}

fn print(plan: &QueryPlan) -> u64 {
    let terms = seek::terms::<Item>(plan).unwrap();
    seek::fingerprint(plan, &terms)
}

#[test]
fn the_fingerprint_follows_the_list_not_the_page() {
    let base = plan().filter(Expr::col("name").eq("a"));
    let same = base.clone().limit(5).offset(10);
    assert_eq!(print(&base), print(&same));
    let other_value = plan().filter(Expr::col("name").eq("b"));
    assert_ne!(print(&base), print(&other_value));
    assert_ne!(print(&base), print(&plan()));
    let desc = base
        .clone()
        .order_by(Expr::col("due"), OrderDirection::Desc);
    let asc = base.clone().order_by(Expr::col("due"), OrderDirection::Asc);
    assert_ne!(print(&desc), print(&asc));
    assert_ne!(print(&base), print(&desc));
    let mut other_table = base.clone();
    other_table.source.name = "others".into();
    assert_ne!(print(&base), print(&other_table));
    assert_eq!(print(&base), print(&base.clone()));
}

fn seek_for(plan: &QueryPlan, values: &[Value], backward: bool) -> Expr {
    let terms = seek::terms::<Item>(plan).unwrap();
    Seek::new(plan, &terms)
        .backwards(backward)
        .predicate(values)
}

fn col(name: &'static str) -> Expr {
    Expr::col(name)
}

#[test]
fn the_predicate_is_the_expanded_or() {
    let by_name = plan().order_by(col("name"), OrderDirection::Asc);
    let values = [Value::Text("b".into()), Value::Int(4)];
    let want = Expr::Or(vec![
        col("name").gt(Expr::val("b")),
        Expr::And(vec![
            col("name").eq(Expr::val("b")),
            col("id").gt(Expr::val(4)),
        ]),
    ]);
    assert_eq!(seek_for(&by_name, &values, false), want);
    let want_back = Expr::Or(vec![
        col("name").lt(Expr::val("b")),
        Expr::And(vec![
            col("name").eq(Expr::val("b")),
            col("id").lt(Expr::val(4)),
        ]),
    ]);
    assert_eq!(seek_for(&by_name, &values, true), want_back);
}

#[test]
fn nulls_are_larger_than_every_value() {
    let asc = plan().order_by(col("due"), OrderDirection::Asc);
    let after_value = [Value::Int(5), Value::Int(2)];
    let want = Expr::Or(vec![
        col("due").gt(Expr::val(5)).or(col("due").is_null(true)),
        Expr::And(vec![
            col("due").eq(Expr::val(5)),
            col("id").gt(Expr::val(2)),
        ]),
    ]);
    assert_eq!(seek_for(&asc, &after_value, false), want);
    // Only equal rows can follow a null when ascending.
    let after_null = [Value::Null, Value::Int(2)];
    let want = Expr::Or(vec![Expr::And(vec![
        col("due").is_null(true),
        col("id").gt(Expr::val(2)),
    ])]);
    assert_eq!(seek_for(&asc, &after_null, false), want);
    // Descending, nulls come first: every value follows a null.
    let desc = plan().order_by(col("due"), OrderDirection::Desc);
    let want = Expr::Or(vec![
        col("due").is_null(false),
        Expr::And(vec![col("due").is_null(true), col("id").gt(Expr::val(2))]),
    ]);
    assert_eq!(seek_for(&desc, &after_null, false), want);
}

#[test]
fn nullable_terms_sort_on_an_is_null_key_first() {
    let by_due = plan().order_by(col("due"), OrderDirection::Desc);
    let terms = seek::terms::<Item>(&by_due).unwrap();
    let order = Seek::new(&plan(), &terms).ordering();
    let shape: Vec<_> = order.iter().map(|o| (&o.expr, o.direction)).collect();
    let (asc, desc) = (OrderDirection::Asc, OrderDirection::Desc);
    let null_key = col("due").is_null(true);
    let want = [(&null_key, desc), (&col("due"), desc), (&col("id"), asc)];
    assert_eq!(shape, want);
    let back = Seek::new(&plan(), &terms).backwards(true).ordering();
    assert!(back[0].direction == asc && back[2].direction == desc);
}

#[test]
fn joins_qualify_the_ordering_columns() {
    let mut joined = plan();
    joined.joins.push(crate::plan::JoinExpr {
        kind: crate::plan::JoinKind::Left,
        source: crate::plan::QuerySource::table("authors"),
        on: col("author_id").eq(col("id")),
    });
    let terms = seek::terms::<Item>(&joined).unwrap();
    let order = Seek::new(&joined, &terms).ordering();
    let want = Expr::Column(crate::expr::Column::qualified("items", "id"));
    assert_eq!(order.last().map(|o| &o.expr), Some(&want));
}
