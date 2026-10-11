//! [`Change`]: one field of a partial update, telling an absent field
//! from `null`.

use crate::context::ValidationContext;
use crate::schema::{Schema, SchemaObject, SchemaRegistry};
use crate::validate::Validate;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// A field of a partial-update (`PATCH`) body: absent, or a new value.
///
/// Mark the field `#[serde(default)]`: an absent field is
/// [`Unchanged`](Self::Unchanged), a present one is [`Set`](Self::Set)
/// with the value `T` reads from the input. `null` is therefore a value:
/// `Change<Option<T>>` holds `Set(None)` for it (clear the column), while
/// for a `T` that rejects `null` (`String`, numbers, ...) the body fails
/// validation. Validation and the schema are `T`'s.
///
/// ```
/// use serde::Deserialize;
/// use siderite_validation::Change;
///
/// #[derive(Deserialize)]
/// struct Patch {
///     #[serde(default)]
///     note: Change<Option<String>>,
/// }
///
/// let absent: Patch = serde_json::from_str("{}").unwrap();
/// assert_eq!(absent.note, Change::Unchanged);
/// let cleared: Patch = serde_json::from_str(r#"{"note":null}"#).unwrap();
/// assert_eq!(cleared.note, Change::Set(None));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Change<T> {
    /// The field was absent: keep the current value.
    #[default]
    Unchanged,
    /// The field was present: the new value.
    Set(T),
}

impl<T> Change<T> {
    /// Store the new value in `target`, if there is one.
    ///
    /// Args:
    ///   target: The value to replace.
    pub fn apply(self, target: &mut T) {
        if let Self::Set(value) = self {
            *target = value;
        }
    }

    /// The new value, if the field was present.
    ///
    /// Returns:
    ///   `Some` for [`Set`](Self::Set), `None` for
    ///   [`Unchanged`](Self::Unchanged).
    pub fn into_option(self) -> Option<T> {
        match self {
            Self::Unchanged => None,
            Self::Set(value) => Some(value),
        }
    }

    /// Whether the field was present.
    pub const fn is_set(&self) -> bool {
        matches!(self, Self::Set(_))
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Change<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        T::deserialize(d).map(Self::Set)
    }
}

impl<T: Validate> Validate for Change<T> {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        // Only present fields reach here: `null` is checked as `T` would.
        T::prepare(input, ctx);
    }

    fn validate(&self, ctx: &mut ValidationContext) {
        if let Self::Set(value) = self {
            value.validate(ctx);
        }
    }
}

impl<T: Schema + 'static> Schema for Change<T> {
    fn schema(registry: &mut SchemaRegistry) -> SchemaObject {
        registry.subschema::<T>()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[derive(Debug, Deserialize)]
    struct Body {
        #[serde(default)]
        name: Change<String>,
        #[serde(default)]
        note: Change<Option<i32>>,
    }

    fn parse(text: &str) -> Result<Body, serde_json::Error> {
        serde_json::from_str(text)
    }

    #[test]
    fn absent_null_and_values() {
        let absent = parse("{}").unwrap();
        assert_eq!(absent.name, Change::Unchanged);
        assert_eq!(absent.note, Change::Unchanged);
        let set = parse(r#"{"name":"x","note":3}"#).unwrap();
        assert_eq!(set.name, Change::Set("x".to_owned()));
        assert_eq!(set.note, Change::Set(Some(3)));
        let cleared = parse(r#"{"note":null}"#).unwrap();
        assert_eq!(cleared.note, Change::Set(None));
        assert!(parse(r#"{"name":null}"#).is_err());
    }

    #[test]
    fn apply_and_accessors() {
        let mut current = 1;
        Change::Unchanged.apply(&mut current);
        assert_eq!(current, 1);
        Change::Set(2).apply(&mut current);
        assert_eq!(current, 2);
        assert_eq!(Change::Set(5).into_option(), Some(5));
        assert_eq!(Change::<i32>::Unchanged.into_option(), None);
        assert!(Change::Set(()).is_set());
    }

    #[test]
    fn the_schema_is_the_inner_type() {
        let mut registry = SchemaRegistry::default();
        let inner = registry.subschema::<i64>();
        let changed = Change::<i64>::schema(&mut registry);
        assert_eq!(changed, inner);
    }
}
