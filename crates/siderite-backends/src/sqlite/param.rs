//! How a [`Value`] is stored in SQLite, shared by the SQLx and native
//! backends so both bind every value the same way.

use siderite_orm::Value;
use siderite_orm::types::canonical_text;

/// A bind parameter in one of SQLite's storage classes.
///
/// Types without a native storage class (decimals, UUIDs, dates, times,
/// timestamps, JSON) become their canonical text ([`canonical_text`]), so
/// they compare and sort correctly and decode through
/// [`DbType::from_value`](siderite_orm::DbType::from_value). Booleans are
/// the integers `0` and `1`.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Stored {
    /// `NULL`.
    Null,
    /// `INTEGER`.
    Integer(i64),
    /// `REAL`.
    Real(f64),
    /// `TEXT`.
    Text(String),
    /// `BLOB`.
    Blob(Vec<u8>),
}

impl From<Value> for Stored {
    fn from(value: Value) -> Self {
        if let Some(text) = canonical_text(&value) {
            return Self::Text(text);
        }
        match value {
            Value::Bool(b) => Self::Integer(i64::from(b)),
            Value::Int(i) => Self::Integer(i),
            Value::Float(f) => Self::Real(f),
            Value::Text(s) => Self::Text(s),
            Value::Bytes(b) => Self::Blob(b),
            // `canonical_text` covered every other variant.
            _ => Self::Null,
        }
    }
}
