//! The cursor token: layout, encoding and validation.
//!
//! A token is unpadded base64url of the version byte, the query
//! fingerprint (`u64`, big-endian), a direction byte and one tagged value
//! per ordering term. Tokens are opaque to clients but not secret, so
//! [`decode`] checks everything before a value reaches a query.

use super::Way;
use crate::error::{CursorError, QueryError};
use crate::types::SqlType;
use crate::value::Value;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, NaiveDate, NaiveTime, TimeDelta, Timelike};
use rust_decimal::Decimal;
use std::str::FromStr;
use uuid::Uuid;

/// Layout version written by this crate.
const VERSION: u8 = 1;
/// Longest accepted token, in characters.
const MAX_TOKEN: usize = 1024;

const NULL: u8 = 0;
const INT: u8 = 1;
const TEXT: u8 = 2;
const FLOAT: u8 = 3;
const BOOL: u8 = 4;
const BYTES: u8 = 5;
const TIMESTAMP: u8 = 6;
const DATE: u8 = 7;
const UUID: u8 = 8;
const DECIMAL: u8 = 9;
const TIME: u8 = 10;

const MICROS: i64 = 1_000_000;

/// A finished token, or why the position cannot be stored.
type Encoded = Result<String, QueryError>;

/// What a cursor value must be for one ordering term.
#[derive(Debug, Clone, Copy)]
pub(super) struct Slot {
    /// Column type of the term.
    pub(super) sql_type: SqlType,
    /// Whether the column accepts `NULL`.
    pub(super) nullable: bool,
}

/// The tag that stores values of column type `ty`; `None` if a column of
/// that type cannot be a cursor position.
pub(super) fn tag_for(ty: SqlType) -> Option<u8> {
    use SqlType as T;
    Some(match ty {
        T::SmallInt | T::Integer | T::BigInt | T::Duration => INT,
        T::Real | T::Double => FLOAT,
        T::Decimal => DECIMAL,
        T::Bool => BOOL,
        T::Text | T::IpAddr => TEXT,
        T::Binary => BYTES,
        T::Date => DATE,
        T::Time => TIME,
        T::Timestamp => TIMESTAMP,
        T::Uuid => UUID,
        T::Json => return None,
    })
}

fn tag_of(value: &Value) -> Option<u8> {
    Some(match value {
        Value::Null => NULL,
        Value::Int(_) => INT,
        Value::Text(_) => TEXT,
        Value::Float(_) => FLOAT,
        Value::Bool(_) => BOOL,
        Value::Bytes(_) => BYTES,
        Value::Timestamp(_) => TIMESTAMP,
        Value::Date(_) => DATE,
        Value::Uuid(_) => UUID,
        Value::Decimal(_) => DECIMAL,
        Value::Time(_) => TIME,
        Value::Json(_) => return None,
    })
}

fn too_large() -> QueryError {
    QueryError::InvalidPlan("an ordering value does not fit a cursor".into())
}

fn put_len(out: &mut Vec<u8>, len: usize) -> Result<(), QueryError> {
    let len = u32::try_from(len).map_err(|_| too_large())?;
    out.extend(len.to_be_bytes());
    Ok(())
}

fn put_value(out: &mut Vec<u8>, value: &Value) -> Result<(), QueryError> {
    out.push(tag_of(value).ok_or_else(too_large)?);
    match value {
        Value::Int(n) => out.extend(n.to_be_bytes()),
        Value::Text(s) => {
            put_len(out, s.len())?;
            out.extend(s.as_bytes());
        }
        Value::Float(f) => out.extend(f.to_bits().to_be_bytes()),
        Value::Bool(b) => out.push(u8::from(*b)),
        Value::Bytes(b) => {
            put_len(out, b.len())?;
            out.extend(b);
        }
        Value::Timestamp(t) => out.extend(t.timestamp_micros().to_be_bytes()),
        Value::Date(d) => {
            let days = d.signed_duration_since(epoch()).num_days();
            let days = i32::try_from(days).map_err(|_| too_large())?;
            out.extend(days.to_be_bytes());
        }
        Value::Uuid(u) => out.extend(u.as_bytes()),
        Value::Decimal(d) => {
            let text = d.to_string();
            put_len(out, text.len())?;
            out.extend(text.as_bytes());
        }
        Value::Time(t) => {
            let secs = i64::from(t.num_seconds_from_midnight());
            let micros = secs * MICROS + i64::from(t.nanosecond() / 1000);
            out.extend(micros.to_be_bytes());
        }
        Value::Null | Value::Json(_) => {}
    }
    Ok(())
}

fn epoch() -> NaiveDate {
    DateTime::UNIX_EPOCH.date_naive()
}

/// The token for a cursor position: `values` are the ordering values of
/// one row, one per term.
///
/// # Errors
/// [`QueryError::InvalidPlan`] if a value cannot be stored or the token
/// would be longer than the 1 KiB the decoder accepts.
pub(super) fn encode(print: u64, way: Way, values: &[Value]) -> Encoded {
    let mut bytes = vec![VERSION];
    bytes.extend(print.to_be_bytes());
    bytes.push(way.byte());
    for value in values {
        put_value(&mut bytes, value)?;
    }
    let token = URL_SAFE_NO_PAD.encode(bytes);
    if token.len() > MAX_TOKEN {
        return Err(too_large());
    }
    Ok(token)
}

/// A cursor over the decoded bytes; every read is bounds-checked.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], CursorError> {
        let end = self.at.checked_add(n).ok_or(CursorError::Malformed)?;
        let slice = self.bytes.get(self.at..end);
        self.at = end;
        slice.ok_or(CursorError::Malformed)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CursorError> {
        let slice = self.take(N)?;
        slice.try_into().map_err(|_| CursorError::Malformed)
    }

    fn byte(&mut self) -> Result<u8, CursorError> {
        Ok(u8::from_be_bytes(self.array()?))
    }

    fn prefixed(&mut self) -> Result<&'a [u8], CursorError> {
        let len = u32::from_be_bytes(self.array()?);
        self.take(usize::try_from(len).map_err(|_| CursorError::Malformed)?)
    }

    fn text(&mut self) -> Result<&'a str, CursorError> {
        let bytes = self.prefixed()?;
        std::str::from_utf8(bytes).map_err(|_| CursorError::Malformed)
    }

    fn value(&mut self) -> Result<Value, CursorError> {
        let bad = CursorError::Malformed;
        Ok(match self.byte()? {
            NULL => Value::Null,
            INT => Value::Int(i64::from_be_bytes(self.array()?)),
            TEXT => Value::Text(self.text()?.to_owned()),
            FLOAT => {
                let bits = u64::from_be_bytes(self.array()?);
                Value::Float(f64::from_bits(bits))
            }
            BOOL => match self.byte()? {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                _ => return Err(bad),
            },
            BYTES => Value::Bytes(self.prefixed()?.to_vec()),
            TIMESTAMP => {
                let micros = i64::from_be_bytes(self.array()?);
                let at = DateTime::from_timestamp_micros(micros);
                Value::Timestamp(at.ok_or(bad)?)
            }
            DATE => {
                let days = i64::from(i32::from_be_bytes(self.array()?));
                let delta = TimeDelta::try_days(days).ok_or(bad)?;
                Value::Date(epoch().checked_add_signed(delta).ok_or(bad)?)
            }
            UUID => Value::Uuid(Uuid::from_bytes(self.array()?)),
            DECIMAL => {
                let parsed = Decimal::from_str(self.text()?);
                Value::Decimal(parsed.map_err(|_| bad)?)
            }
            TIME => {
                let micros = i64::from_be_bytes(self.array()?);
                let secs = u32::try_from(micros.div_euclid(MICROS));
                let nanos = u32::try_from(micros.rem_euclid(MICROS) * 1000);
                let time = NaiveTime::from_num_seconds_from_midnight_opt(
                    secs.map_err(|_| bad)?,
                    nanos.map_err(|_| bad)?,
                );
                Value::Time(time.ok_or(bad)?)
            }
            _ => return Err(bad),
        })
    }
}

/// Check `token` and return its values.
///
/// The checks run in a fixed order, so each defect has exactly one code:
/// length and base64url, version, fingerprint, direction, the value
/// encoding, then the count and types against `slots`. A null fits a
/// nullable term only.
///
/// # Errors
/// [`CursorError`] naming the first defect.
pub(super) fn decode(
    token: &str,
    fingerprint: u64,
    way: Way,
    slots: &[Slot],
) -> Result<Vec<Value>, CursorError> {
    if token.len() > MAX_TOKEN {
        return Err(CursorError::Malformed);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| CursorError::Malformed)?;
    let mut reader = Reader {
        bytes: &bytes,
        at: 0,
    };
    if reader.byte()? != VERSION {
        return Err(CursorError::UnknownVersion);
    }
    if u64::from_be_bytes(reader.array()?) != fingerprint {
        return Err(CursorError::ForeignQuery);
    }
    if reader.byte()? != way.byte() {
        return Err(CursorError::Malformed);
    }
    let mut values = Vec::new();
    while reader.at < bytes.len() {
        values.push(reader.value()?);
    }
    let fits = values.len() == slots.len()
        && values.iter().zip(slots).all(|(value, slot)| {
            if value.is_null() {
                slot.nullable
            } else {
                tag_of(value) == tag_for(slot.sql_type)
            }
        });
    if fits {
        Ok(values)
    } else {
        Err(CursorError::Mismatch)
    }
}
