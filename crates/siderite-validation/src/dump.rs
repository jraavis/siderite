//! Serialization options (Pydantic `model_dump` equivalents).
//!
//! siderite composes with Serde instead of replacing it: a value is first
//! serialized by Serde, then [`DumpOptions`] are applied. `#[derive(Schema)]`
//! implements [`Dump`] for models to add computed fields and field/model
//! serializers and to recurse into nested models, so nested computed fields
//! also appear.

use serde::{Serialize, Serializer};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
use thiserror::Error;

/// Serialization failure.
#[derive(Debug, Error)]
#[error("serialization failed: {0}")]
pub struct DumpError(#[from] pub serde_json::Error);

/// A tree of field names, for `include` / `exclude`.
///
/// A leaf selects the whole field; children select nested fields. Nested
/// selections apply to every element of lists and maps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldSet(BTreeMap<String, FieldSet>);

impl FieldSet {
    /// Empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set of top-level fields.
    pub fn of<I: IntoIterator<Item = S>, S: Into<String>>(fields: I) -> Self {
        Self(
            fields
                .into_iter()
                .map(|f| (f.into(), Self::new()))
                .collect(),
        )
    }

    /// Add a nested selection `field -> children`.
    #[must_use]
    pub fn nested(mut self, field: impl Into<String>, children: FieldSet) -> Self {
        self.0.insert(field.into(), children);
        self
    }

    fn get(&self, field: &str) -> Option<&FieldSet> {
        self.0.get(field)
    }

    fn is_leaf(&self) -> bool {
        self.0.is_empty()
    }
}

/// Options controlling serialization output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DumpOptions {
    /// Drop object members whose value is `null`.
    pub exclude_none: bool,
    /// Drop fields equal to their declared default (derived models only).
    pub exclude_defaults: bool,
    /// Keep only these fields.
    pub include: Option<FieldSet>,
    /// Drop these fields.
    pub exclude: Option<FieldSet>,
}

impl DumpOptions {
    /// Default options (plain Serde output).
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop `null` members.
    #[must_use]
    pub fn exclude_none(mut self) -> Self {
        self.exclude_none = true;
        self
    }

    /// Drop fields equal to their default.
    #[must_use]
    pub fn exclude_defaults(mut self) -> Self {
        self.exclude_defaults = true;
        self
    }

    /// Keep only `fields`.
    #[must_use]
    pub fn include(mut self, fields: FieldSet) -> Self {
        self.include = Some(fields);
        self
    }

    /// Drop `fields`.
    #[must_use]
    pub fn exclude(mut self, fields: FieldSet) -> Self {
        self.exclude = Some(fields);
        self
    }

    /// Options for the member `field` of an object, or `None` if the member
    /// is excluded entirely.
    pub fn for_field(&self, field: &str) -> Option<DumpOptions> {
        let include = match &self.include {
            None => None,
            Some(set) => match set.get(field) {
                None => return None,
                Some(child) if child.is_leaf() => None,
                Some(child) => Some(child.clone()),
            },
        };
        let exclude = match &self.exclude {
            None => None,
            Some(set) => match set.get(field) {
                Some(child) if child.is_leaf() => return None,
                Some(child) => Some(child.clone()),
                None => None,
            },
        };
        Some(DumpOptions {
            include,
            exclude,
            ..self.clone()
        })
    }

    /// Whether a member with this value should be dropped by `exclude_none`.
    pub fn drops_value(&self, value: &Value) -> bool {
        self.exclude_none && value.is_null()
    }

    /// Apply the options to an already-serialized value (used for types
    /// without derived dumping).
    pub fn apply(&self, value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut out = Map::new();
                for (key, member) in map {
                    let Some(child) = self.for_field(&key) else {
                        continue;
                    };
                    let member = child.apply(member);
                    if !self.drops_value(&member) {
                        out.insert(key, member);
                    }
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.into_iter().map(|v| self.apply(v)).collect()),
            other => other,
        }
    }
}

/// Output serialization with [`DumpOptions`] (Pydantic `model_dump`).
///
/// The default serializes with Serde and post-processes the result, so a
/// hand-written type opts in with `impl Dump for T {}`. `#[derive(Schema)]`
/// implements it for models: computed fields, field/model serializers,
/// `exclude_defaults`, and recursion into nested models. Containers delegate
/// to their elements so nested models keep their computed fields.
pub trait Dump: Serialize {
    /// Serialize directly when supported, preserving `dump` semantics.
    ///
    /// Args:
    ///     serializer: Destination serializer.
    ///     opts: Field selection and exclusion policy.
    ///
    /// Returns:
    ///     Serialized output, or a serialization error.
    fn serialize_dump<S: Serializer>(
        &self,
        serializer: S,
        opts: &DumpOptions,
    ) -> Result<S::Ok, S::Error> {
        self.dump(opts)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }

    /// Serialize with `opts` applied.
    ///
    /// # Errors
    /// Serialization failures.
    fn dump(&self, opts: &DumpOptions) -> Result<Value, DumpError> {
        Ok(opts.apply(serde_json::to_value(self)?))
    }
}

/// Borrowed Serde adapter that preserves a value's `Dump` implementation.
pub struct DumpSerialize<'a, T: ?Sized>(pub &'a T, pub &'a DumpOptions);

impl<T: Dump + ?Sized> Serialize for DumpSerialize<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize_dump(serializer, self.1)
    }
}

macro_rules! plain_dump {
    ($($t:ty),* $(,)?) => {$(
        impl Dump for $t {
            fn serialize_dump<S: Serializer>(
                &self,
                serializer: S,
                _: &DumpOptions,
            ) -> Result<S::Ok, S::Error> {
                self.serialize(serializer)
            }
        }
    )*};
}
plain_dump!(
    bool,
    i8,
    i16,
    i32,
    i64,
    u8,
    u16,
    u32,
    u64,
    isize,
    usize,
    char,
    String,
    str,
    (),
);

impl Dump for f32 {}
impl Dump for f64 {}
impl Dump for i128 {}
impl Dump for u128 {}
impl Dump for Value {}
impl Dump for Map<String, Value> {}

impl<T: Dump + ?Sized> Dump for &T {
    fn serialize_dump<S: Serializer>(
        &self,
        serializer: S,
        opts: &DumpOptions,
    ) -> Result<S::Ok, S::Error> {
        (**self).serialize_dump(serializer, opts)
    }

    fn dump(&self, opts: &DumpOptions) -> Result<Value, DumpError> {
        (**self).dump(opts)
    }
}

impl<T: Dump + ?Sized> Dump for Box<T> {
    fn serialize_dump<S: Serializer>(
        &self,
        serializer: S,
        opts: &DumpOptions,
    ) -> Result<S::Ok, S::Error> {
        (**self).serialize_dump(serializer, opts)
    }

    fn dump(&self, opts: &DumpOptions) -> Result<Value, DumpError> {
        (**self).dump(opts)
    }
}

impl<T: Dump> Dump for Option<T> {
    fn serialize_dump<S: Serializer>(
        &self,
        serializer: S,
        opts: &DumpOptions,
    ) -> Result<S::Ok, S::Error> {
        match self {
            Some(value) => value.serialize_dump(serializer, opts),
            None => serializer.serialize_none(),
        }
    }

    fn dump(&self, opts: &DumpOptions) -> Result<Value, DumpError> {
        self.as_ref()
            .map_or(Ok(Value::Null), |inner| inner.dump(opts))
    }
}

impl<T: Dump> Dump for [T] {
    fn serialize_dump<S: Serializer>(
        &self,
        serializer: S,
        opts: &DumpOptions,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(Some(self.len()))?;
        for value in self {
            sequence.serialize_element(&DumpSerialize(value, opts))?;
        }
        sequence.end()
    }

    fn dump(&self, opts: &DumpOptions) -> Result<Value, DumpError> {
        self.iter()
            .map(|item| item.dump(opts))
            .collect::<Result<_, _>>()
            .map(Value::Array)
    }
}

impl<T: Dump> Dump for Vec<T> {
    fn serialize_dump<S: Serializer>(
        &self,
        serializer: S,
        opts: &DumpOptions,
    ) -> Result<S::Ok, S::Error> {
        self.as_slice().serialize_dump(serializer, opts)
    }

    fn dump(&self, opts: &DumpOptions) -> Result<Value, DumpError> {
        self.as_slice().dump(opts)
    }
}

fn dump_map<'a, T: Dump + 'a>(
    entries: impl Iterator<Item = (&'a String, &'a T)>,
    opts: &DumpOptions,
) -> Result<Value, DumpError> {
    let mut out = Map::new();
    for (key, value) in entries {
        let value = value.dump(opts)?;
        if !opts.drops_value(&value) {
            out.insert(key.clone(), value);
        }
    }
    Ok(Value::Object(out))
}

impl<T: Dump> Dump for BTreeMap<String, T> {
    fn dump(&self, opts: &DumpOptions) -> Result<Value, DumpError> {
        dump_map(self.iter(), opts)
    }
}

impl<T: Dump, S: std::hash::BuildHasher> Dump for HashMap<String, T, S> {
    fn dump(&self, opts: &DumpOptions) -> Result<Value, DumpError> {
        dump_map(self.iter(), opts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exclude_none_and_nested_exclude() {
        let opts = DumpOptions::new()
            .exclude_none()
            .exclude(FieldSet::new().nested("user", FieldSet::of(["password"])));
        let out = opts.apply(json!({"a": null, "user": {"name": "x", "password": "p"}, "b": 1}));
        assert_eq!(out, json!({"user": {"name": "x"}, "b": 1}));
    }

    #[test]
    fn include_applies_through_lists() {
        let opts =
            DumpOptions::new().include(FieldSet::new().nested("items", FieldSet::of(["id"])));
        let out = opts.apply(json!({"items": [{"id": 1, "x": 2}], "other": 3}));
        assert_eq!(out, json!({"items": [{"id": 1}]}));
    }
}
