//! The cursor pagination contract (`docs/plans/CURSOR_PAGINATION.md`),
//! run against a reference model.
//!
//! Nothing here uses an implementation: the fixtures in
//! `cursor_contract.json` state the pages and token errors an
//! implementation must produce, and this file checks them against the
//! design's rules (total ordering with the primary key, the null
//! positions, the expanded seek predicate, one `LIMIT n + 1` fetch per
//! page and the token layout). A future implementation runs the same
//! fixtures against each backend.

#![allow(clippy::unwrap_used)]

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::BTreeSet;

const FIXTURES: &str = include_str!("cursor_contract.json");

#[derive(Deserialize)]
struct Fixtures {
    format: u32,
    cases: Vec<Case>,
    tokens: Vec<TokenCase>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    rows: Vec<Map<String, Value>>,
    order: Vec<Term>,
    pages: Vec<PageCase>,
}

#[derive(Deserialize, Clone)]
struct Term {
    field: String,
    dir: String,
}

#[derive(Deserialize)]
struct PageCase {
    #[serde(default)]
    before: Option<Writes>,
    limit: usize,
    ids: Vec<i64>,
    more: bool,
}

#[derive(Deserialize)]
struct Writes {
    #[serde(default)]
    insert: Vec<Map<String, Value>>,
    #[serde(default)]
    delete: Vec<i64>,
}

#[derive(Deserialize)]
struct TokenCase {
    name: String,
    edit: String,
    error: Option<String>,
}

type Row = Map<String, Value>;

fn fixtures() -> Fixtures {
    let parsed: Fixtures = serde_json::from_str(FIXTURES).unwrap();
    assert_eq!(parsed.format, 1);
    parsed
}

/// The ordering terms with the primary key appended when absent.
fn total_order(order: &[Term]) -> Vec<Term> {
    let mut terms = order.to_vec();
    if !terms.iter().any(|t| t.field == "id") {
        terms.push(Term {
            field: "id".to_owned(),
            dir: "asc".to_owned(),
        });
    }
    terms
}

fn value<'a>(row: &'a Row, field: &str) -> &'a Value {
    row.get(field).unwrap_or(&Value::Null)
}

fn int(v: &Value) -> i64 {
    v.as_i64().unwrap()
}

fn id(row: &Row) -> i64 {
    int(&row["id"])
}

/// Ascending comparison of two non-null values of one type.
fn compare(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(_), Value::Number(_)) => int(a).cmp(&int(b)),
        (Value::String(x), Value::String(y)) => x.as_bytes().cmp(y.as_bytes()),
        _ => panic!("ordering values of different types: {a} and {b}"),
    }
}

/// The design's sort: nulls last ascending, first descending.
fn sort(rows: &mut [Row], terms: &[Term]) {
    rows.sort_by(|a, b| {
        for t in terms {
            let (x, y) = (value(a, &t.field), value(b, &t.field));
            let ascending = match (x.is_null(), y.is_null()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => compare(x, y),
            };
            let ordered = if t.dir == "desc" {
                ascending.reverse()
            } else {
                ascending
            };
            if ordered != Ordering::Equal {
                return ordered;
            }
        }
        Ordering::Equal
    });
}

/// Term equality as the predicate writes it (`t = v` or `t IS NULL`).
fn equal(t: &Value, v: &Value) -> bool {
    match (t.is_null(), v.is_null()) {
        (true, true) => true,
        (false, false) => compare(t, v) == Ordering::Equal,
        _ => false,
    }
}

/// Whether `t` is past the cursor value `v` in direction `dir`.
fn past(t: &Value, v: &Value, dir: &str) -> bool {
    match (dir, t.is_null(), v.is_null()) {
        // Ascending: null is last, so nothing is past a null cursor.
        ("asc", _, true) => false,
        ("asc", true, false) => true,
        ("asc", false, false) => compare(t, v) == Ordering::Greater,
        // Descending: null is first, so every value is past it.
        (_, false, true) => true,
        (_, true, false) => false,
        (_, false, false) => compare(t, v) == Ordering::Less,
        _ => unreachable!(),
    }
}

/// The expanded seek predicate: some term is past the cursor while the
/// terms before it are equal.
fn after(row: &Row, cursor: &[Value], terms: &[Term]) -> bool {
    (0..terms.len()).any(|k| {
        let prefix = terms[..k]
            .iter()
            .zip(cursor)
            .all(|(t, v)| equal(value(row, &t.field), v));
        let t = &terms[k];
        prefix && past(value(row, &t.field), &cursor[k], &t.dir)
    })
}

/// A page and whether more rows follow.
type Fetched = (Vec<Row>, bool);
/// The ordering values of the last row of the previous page.
type Cursor<'a> = Option<&'a [Value]>;

/// One page: rows past the cursor, sorted, `LIMIT n + 1`.
fn fetch(rows: &[Row], at: Cursor<'_>, terms: &[Term], n: usize) -> Fetched {
    let mut found: Vec<Row> = rows
        .iter()
        .filter(|r| at.is_none_or(|c| after(r, c, terms)))
        .cloned()
        .collect();
    sort(&mut found, terms);
    found.truncate(n + 1);
    let more = found.len() > n;
    found.truncate(n);
    (found, more)
}

fn ids(rows: &[Row]) -> Vec<i64> {
    rows.iter().map(id).collect()
}

#[test]
fn pages_follow_the_seek_contract() {
    for case in fixtures().cases {
        let terms = total_order(&case.order);
        let mut rows = case.rows.clone();
        let mut cursor: Option<Vec<Value>> = None;
        let mut seen = Vec::new();
        for (n, page) in case.pages.iter().enumerate() {
            if let Some(writes) = &page.before {
                let gone = &writes.delete;
                rows.retain(|r| !gone.contains(&id(r)));
                rows.extend(writes.insert.iter().cloned());
            }
            let at = cursor.as_deref();
            let (found, more) = fetch(&rows, at, &terms, page.limit);
            let label = format!("{} page {}", case.name, n + 1);
            assert_eq!(ids(&found), page.ids, "{label}");
            assert_eq!(more, page.more, "{label}");
            let last = found.last().unwrap();
            cursor = Some(
                terms
                    .iter()
                    .map(|t| value(last, &t.field).clone())
                    .collect(),
            );
            seen.extend(ids(&found));
        }
        let unique: BTreeSet<i64> = seen.iter().copied().collect();
        assert_eq!(unique.len(), seen.len(), "{}: no row twice", case.name);
        if case.pages.iter().all(|p| p.before.is_none()) {
            // Without writes, the pages are the sorted rows in order.
            let mut sorted = case.rows.clone();
            sort(&mut sorted, &terms);
            let want = ids(&sorted);
            assert_eq!(seen, want, "{}: seek equals slicing", case.name);
        }
    }
}

// ---------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------

const VERSION: u8 = 1;
const MAX_TOKEN: usize = 1024;

#[derive(Debug, Clone, PartialEq)]
enum Typed {
    Null,
    Int(i64),
    Text(String),
}

fn encode(version: u8, fingerprint: u64, values: &[Typed]) -> String {
    let mut bytes = vec![version];
    bytes.extend(fingerprint.to_be_bytes());
    bytes.push(b'a');
    for v in values {
        match v {
            Typed::Null => bytes.push(0),
            Typed::Int(n) => {
                bytes.push(1);
                bytes.extend(n.to_be_bytes());
            }
            Typed::Text(s) => {
                bytes.push(2);
                let len = u32::try_from(s.len()).unwrap();
                bytes.extend(len.to_be_bytes());
                bytes.extend(s.as_bytes());
            }
        }
    }
    URL_SAFE_NO_PAD.encode(bytes)
}

type Decoded = Result<Vec<Typed>, &'static str>;

/// A fixed-size array from a slice of that length.
fn be<const N: usize>(slice: &[u8]) -> [u8; N] {
    slice.try_into().unwrap()
}

/// Column types of the ordering: `1` integer, `2` text.
type Columns<'a> = &'a [u8];

fn decode(token: &str, fingerprint: u64, columns: Columns) -> Decoded {
    const MALFORMED: &str = "cursor.E001";
    if token.len() > MAX_TOKEN {
        return Err(MALFORMED);
    }
    let bytes = URL_SAFE_NO_PAD.decode(token).map_err(|_| MALFORMED)?;
    let take = |at: &mut usize, n: usize| -> Result<&[u8], &'static str> {
        let slice = bytes.get(*at..*at + n).ok_or(MALFORMED)?;
        *at += n;
        Ok(slice)
    };
    let mut at = 0;
    if take(&mut at, 1)?[0] != VERSION {
        return Err("cursor.E002");
    }
    let found = u64::from_be_bytes(be(take(&mut at, 8)?));
    if found != fingerprint {
        return Err("cursor.E003");
    }
    if take(&mut at, 1)?[0] != b'a' {
        return Err(MALFORMED);
    }
    let mut values = Vec::new();
    let mut tags = Vec::new();
    while at < bytes.len() {
        let tag = take(&mut at, 1)?[0];
        let value = match tag {
            0 => Typed::Null,
            1 => Typed::Int(i64::from_be_bytes(be(take(&mut at, 8)?))),
            2 => {
                let len = u32::from_be_bytes(be(take(&mut at, 4)?));
                let text = take(&mut at, usize::try_from(len).unwrap())?;
                let text = String::from_utf8(text.to_vec());
                Typed::Text(text.map_err(|_| MALFORMED)?)
            }
            _ => return Err(MALFORMED),
        };
        tags.push(tag);
        values.push(value);
    }
    // A null fits any column; other values must match its type.
    let typed = tags.iter().zip(columns).all(|(t, c)| *t == 0 || t == c);
    let fits = values.len() == columns.len() && typed;
    if !fits {
        return Err("cursor.E004");
    }
    Ok(values)
}

#[test]
fn tokens_are_validated_and_versioned() {
    const FINGERPRINT: u64 = 0x5eed_cafe_f00d_0001;
    let columns: Columns = &[1, 1];
    let values = [Typed::Int(20), Typed::Int(5)];
    let valid = encode(VERSION, FINGERPRINT, &values);
    for case in fixtures().tokens {
        let token = match case.edit.as_str() {
            "none" => valid.clone(),
            "garbage" => "@@not base64@@".to_owned(),
            "oversize" => format!("{valid}{}", "A".repeat(MAX_TOKEN)),
            "truncate" => valid[..valid.len() - 4].to_owned(),
            "version" => encode(VERSION + 1, FINGERPRINT, &values),
            "fingerprint" => encode(VERSION, FINGERPRINT + 1, &values),
            "extra_value" => {
                let more = [Typed::Int(20), Typed::Int(5), Typed::Null];
                encode(VERSION, FINGERPRINT, &more)
            }
            "wrong_type" => {
                let text = [Typed::Text("20".to_owned()), Typed::Int(5)];
                encode(VERSION, FINGERPRINT, &text)
            }
            other => panic!("unknown edit {other}"),
        };
        let decoded = decode(&token, FINGERPRINT, columns);
        match &case.error {
            None => {
                let want = Ok(&values[..]);
                assert_eq!(decoded.as_deref(), want, "{}", case.name);
            }
            Some(code) => {
                let found = decoded.err();
                assert_eq!(found, Some(code.as_str()), "{}", case.name);
            }
        }
    }
    // A null cursor value fits any column.
    let nulls = encode(VERSION, FINGERPRINT, &[Typed::Null, Typed::Int(1)]);
    assert!(decode(&nulls, FINGERPRINT, columns).is_ok());
}
