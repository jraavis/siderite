//! Position plans against reordered, partial, duplicated and prefixed
//! headers decode exactly as the by-name path does.

use super::*;
use crate::model::{FieldMeta, take_column};
use crate::plan::OrderDirection;
use crate::types::SqlType;

#[derive(Debug, Clone, PartialEq)]
struct Pair {
    id: i64,
    name: String,
}

static PAIR_FIELDS: [FieldMeta; 2] = [
    FieldMeta {
        primary_key: true,
        ..FieldMeta::new("id", "id", SqlType::BigInt)
    },
    FieldMeta::new("name", "name", SqlType::Text),
];

static PAIR_META: ModelMeta = ModelMeta {
    name: "Pair",
    table: "pairs",
    fields: &PAIR_FIELDS,
    many_to_many: &[],
    ordering: &[("id", OrderDirection::Asc)],
    indexes: &[],
    constraints: &[],
    managed: true,
};

/// What `#[derive(Model)]` generates for `Pair`, with both decoders.
impl Model for Pair {
    type Pk = i64;
    const META: &'static ModelMeta = &PAIR_META;

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
        vec![
            ("id", Value::Int(self.id)),
            ("name", Value::Text(self.name.clone())),
        ]
    }

    fn from_row(row: &Row, prefix: &str) -> Result<Self, QueryError> {
        Self::from_row_mut(&mut row.clone(), prefix)
    }

    fn from_row_mut(row: &mut Row, prefix: &str) -> Result<Self, QueryError> {
        Ok(Self {
            id: take_column(row, prefix, "id", 0)?,
            name: take_column(row, prefix, "name", 1)?,
        })
    }

    fn from_cells(cells: &mut dyn Cells, plan: &ColumnPlan) -> Result<Self, QueryError> {
        Ok(Self {
            id: take_planned(cells, plan, "id", 0)?,
            name: take_planned(cells, plan, "name", 1)?,
        })
    }
}

/// A hand-written model that only implements `from_row`.
#[derive(Debug, PartialEq)]
struct Legacy {
    name: String,
}

impl Model for Legacy {
    type Pk = i64;
    const META: &'static ModelMeta = &PAIR_META;

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

    fn from_row(row: &Row, prefix: &str) -> Result<Self, QueryError> {
        Ok(Self {
            name: crate::model::read_column(row, prefix, "name")?,
        })
    }
}

fn row(columns: &[(&str, Value)]) -> Row {
    Row::new(
        columns
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect(),
    )
}

/// Decode `row` through a plan and through `from_row_mut`; both must agree.
fn both(row: &Row, prefix: &str) -> Result<Pair, QueryError> {
    let plan = ColumnPlan::new(&PAIR_META, row.names(), prefix);
    let planned = Pair::from_cells(&mut row.clone(), &plan);
    let by_name = Pair::from_row_mut(&mut row.clone(), prefix);
    assert_eq!(planned, by_name, "plan and by-name decoding disagree");
    planned
}

fn pair(id: i64, name: &str) -> Pair {
    Pair {
        id,
        name: name.into(),
    }
}

#[test]
fn reordered_and_extra_columns_are_found_by_name() {
    let reordered = row(&[("name", Value::Text("a".into())), ("id", Value::Int(1))]);
    assert_eq!(both(&reordered, ""), Ok(pair(1, "a")));
    let annotated = row(&[
        ("id", Value::Int(2)),
        ("n_books", Value::Int(7)),
        ("name", Value::Text("b".into())),
    ]);
    assert_eq!(both(&annotated, ""), Ok(pair(2, "b")));
}

#[test]
fn a_missing_column_fails_like_the_by_name_path() {
    let partial = row(&[("id", Value::Int(1))]);
    let err = both(&partial, "");
    assert_eq!(
        err,
        Err(QueryError::Decode {
            column: "name".into(),
            reason: "column missing from result".into(),
        })
    );
}

#[test]
fn a_mistyped_column_names_the_column_and_reason() {
    let mistyped = row(&[("id", Value::Text("x".into())), ("name", Value::Null)]);
    let Err(QueryError::Decode { column, reason }) = both(&mistyped, "") else {
        panic!("expected a decode error");
    };
    assert_eq!(column, "id");
    assert!(!reason.is_empty());
}

#[test]
fn duplicate_names_resolve_as_row_position_does() {
    // `name` is field 1: its hint wins over the first `name`.
    let hinted = row(&[
        ("name", Value::Text("first".into())),
        ("name", Value::Text("second".into())),
        ("id", Value::Int(3)),
    ]);
    assert_eq!(both(&hinted, ""), Ok(pair(3, "second")));
    // Off its hint, the first column of that name is taken.
    let first = row(&[
        ("x", Value::Int(0)),
        ("id", Value::Int(4)),
        ("id", Value::Int(5)),
        ("name", Value::Text("c".into())),
    ]);
    assert_eq!(both(&first, ""), Ok(pair(4, "c")));
}

#[test]
fn prefixed_columns_decode_a_joined_model() {
    let joined = row(&[
        ("id", Value::Int(1)),
        ("name", Value::Text("root".into())),
        ("author__id", Value::Int(9)),
        ("author__name", Value::Text("joined".into())),
    ]);
    assert_eq!(both(&joined, "author__"), Ok(pair(9, "joined")));
    let partial = row(&[("author__id", Value::Int(9))]);
    assert_eq!(
        both(&partial, "author__"),
        Err(QueryError::Decode {
            column: "author__name".into(),
            reason: "column missing from result".into(),
        })
    );
}

#[test]
fn hand_written_models_fall_back_to_from_row() {
    let joined = row(&[
        ("name", Value::Text("root".into())),
        ("author__name", Value::Text("joined".into())),
    ]);
    let plan = ColumnPlan::new(&PAIR_META, joined.names(), "author__");
    let decoded = Legacy::from_cells(&mut joined.clone(), &plan);
    assert_eq!(
        decoded,
        Ok(Legacy {
            name: "joined".into()
        })
    );
    let mut sink = ModelSink::<Legacy>::new();
    assert_eq!(sink.push(&mut joined.clone()), Ok(()));
    assert_eq!(
        sink.into_models(),
        vec![Legacy {
            name: "root".into()
        }]
    );
}

#[test]
fn the_sink_replans_for_each_header() {
    let mut sink = ModelSink::<Pair>::new();
    let first = row(&[("id", Value::Int(1)), ("name", Value::Text("a".into()))]);
    let header = Arc::clone(first.names());
    let again = Row::with_columns(header, vec![Value::Int(2), Value::Text("b".into())]);
    let other = row(&[("name", Value::Text("c".into())), ("id", Value::Int(3))]);
    for mut row in [first, again, other] {
        assert_eq!(sink.push(&mut row), Ok(()));
    }
    let plan = sink.plan.as_ref().map(|plan| plan.slots.clone());
    assert_eq!(
        plan,
        Some(vec![(Some(1), "id".into()), (Some(0), "name".into())])
    );
    assert_eq!(
        sink.into_models(),
        vec![pair(1, "a"), pair(2, "b"), pair(3, "c")]
    );
}

#[test]
fn a_plan_matches_only_its_own_header() {
    let a = row(&[("id", Value::Int(1)), ("name", Value::Null)]);
    let b = a.clone();
    let c = row(&[("id", Value::Int(1)), ("name", Value::Null)]);
    let plan = ColumnPlan::new(&PAIR_META, a.names(), "");
    assert!(plan.is_for(b.names()));
    assert!(!plan.is_for(c.names()));
    assert_eq!(plan.prefix(), "");
}

#[test]
fn a_field_outside_the_plan_reads_as_missing() {
    let plan = ColumnPlan::new(&PAIR_META, row(&[]).names(), "p__");
    let got = take_planned::<i64>(&mut row(&[]), &plan, "extra", 5);
    assert_eq!(
        got,
        Err(QueryError::Decode {
            column: "p__extra".into(),
            reason: "column missing from result".into(),
        })
    );
}
