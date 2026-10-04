//! Model hooks: validators, computed fields and serializers.
//!
//! `#[model_hooks] impl User { ... }` implements [`ModelHooks`] from
//! `#[field_validator]`, `#[model_validator]`, `#[computed_field]`,
//! `#[field_serializer]` and `#[model_serializer]` methods. The
//! `Validate`/`Schema` derives call the hooks **without** an opt-in flag via
//! [`Probe`] (autoref specialisation): if the model implements
//! `ModelHooks` the hooks run, otherwise the calls compile to no-ops.
//!
//! Probing resolves at the call site in generated code, so it only works when
//! the model type is concrete. Generic models (`Page<T>`) that have hooks must
//! say so with `#[model_config(hooks)]`, which makes the derive call
//! `<Self as ModelHooks>` directly.
//!
//! # Execution order (deterministic)
//! 1. `before_model` on the raw object (model `mode = "before"`).
//! 2. For each field in declaration order: `before_field`, then the field
//!    type's own `prepare` (coercion, constraints on the raw value).
//! 3. Deserialization.
//! 4. Field constraints, then `after_fields` (field `mode = "after"`
//!    validators, declaration order), then `after_model`.

use crate::context::ValidationContext;
use crate::dump::{DumpError, DumpOptions};
use crate::schema::SchemaRegistry;
use serde_json::{Map, Value};
use std::marker::PhantomData;

/// Hooks a model can provide. Every method defaults to doing nothing.
pub trait ModelHooks {
    /// Model before-validators, run on the raw input object.
    fn before_model(_input: &mut Value, _ctx: &mut ValidationContext) {}

    /// Field before-validators for the field whose Rust name is `field`,
    /// run on its raw input slot (location already set to the field).
    fn before_field(_field: &str, _input: &mut Value, _ctx: &mut ValidationContext) {}

    /// Field after-validators (each reports at its field's location).
    fn after_fields(&self, _ctx: &mut ValidationContext) {}

    /// Model after-validators.
    fn after_model(&self, _ctx: &mut ValidationContext) {}

    /// Insert computed fields into the serialized object.
    ///
    /// # Errors
    /// Serialization failures of computed values.
    fn computed_fields(
        &self,
        _out: &mut Map<String, Value>,
        _opts: &DumpOptions,
    ) -> Result<(), DumpError> {
        Ok(())
    }

    /// Document computed fields (added as `readOnly` properties).
    fn computed_schema(_properties: &mut Map<String, Value>, _registry: &mut SchemaRegistry) {}

    /// Field serializer for the field whose Rust name is `field`.
    ///
    /// # Errors
    /// Serialization failures.
    fn serialize_field(&self, _field: &str, value: Value) -> Result<Value, DumpError> {
        Ok(value)
    }

    /// Model serializer applied to the finished object.
    ///
    /// # Errors
    /// Serialization failures.
    fn serialize_model(&self, value: Value) -> Result<Value, DumpError> {
        Ok(value)
    }
}

/// Zero-sized probe used by generated code: call hooks on `&&Probe::<T>::new()`.
#[doc(hidden)]
pub struct Probe<T: ?Sized>(PhantomData<fn() -> PhantomData<T>>);

impl<T: ?Sized> Probe<T> {
    /// New probe.
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T: ?Sized> Default for Probe<T> {
    fn default() -> Self {
        Self::new()
    }
}

macro_rules! probe_traits {
    ($($(#[$doc:meta])* fn $name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)? => |$($d:ident),*| $default:expr;)*) => {
        /// Selected when the model implements [`ModelHooks`].
        #[doc(hidden)]
        pub trait ViaHooks<T: ?Sized> {
            $($(#[$doc])* fn $name(&self, $($arg: $ty),*) $(-> $ret)?;)*
        }

        /// Fallback when the model has no hooks.
        #[doc(hidden)]
        pub trait ViaDefault<T: ?Sized> {
            $($(#[$doc])* fn $name(&self, $($arg: $ty),*) $(-> $ret)?;)*
        }

        impl<T: ?Sized> ViaDefault<T> for Probe<T> {
            $(
                #[allow(unused_variables)]
                fn $name(&self, $($arg: $ty),*) $(-> $ret)? {
                    let ($($d,)*) = ($($d,)*);
                    $default
                }
            )*
        }
    };
}

probe_traits! {
    /// Whether the model implements hooks, requiring value-based dumping.
    fn has_hooks(model: &T) -> bool => |model| false;
    /// Probe for [`ModelHooks::before_model`].
    fn before_model(input: &mut Value, ctx: &mut ValidationContext) => |input, ctx| ();
    /// Probe for [`ModelHooks::before_field`].
    fn before_field(field: &str, input: &mut Value, ctx: &mut ValidationContext) => |field, input, ctx| ();
    /// Probe for [`ModelHooks::after_fields`].
    fn after_fields(model: &T, ctx: &mut ValidationContext) => |model, ctx| ();
    /// Probe for [`ModelHooks::after_model`].
    fn after_model(model: &T, ctx: &mut ValidationContext) => |model, ctx| ();
    /// Probe for [`ModelHooks::computed_fields`].
    fn computed_fields(model: &T, out: &mut Map<String, Value>, opts: &DumpOptions) -> Result<(), DumpError> => |model, out, opts| Ok(());
    /// Probe for [`ModelHooks::computed_schema`].
    fn computed_schema(properties: &mut Map<String, Value>, registry: &mut SchemaRegistry) => |properties, registry| ();
    /// Probe for [`ModelHooks::serialize_field`].
    fn serialize_field(model: &T, field: &str, value: Value) -> Result<Value, DumpError> => |model, field, value| Ok(value);
    /// Probe for [`ModelHooks::serialize_model`].
    fn serialize_model(model: &T, value: Value) -> Result<Value, DumpError> => |model, value| Ok(value);
}

impl<T: ModelHooks> ViaHooks<T> for &Probe<T> {
    fn has_hooks(&self, _: &T) -> bool {
        true
    }

    fn before_model(&self, input: &mut Value, ctx: &mut ValidationContext) {
        T::before_model(input, ctx);
    }
    fn before_field(&self, field: &str, input: &mut Value, ctx: &mut ValidationContext) {
        T::before_field(field, input, ctx);
    }
    fn after_fields(&self, model: &T, ctx: &mut ValidationContext) {
        model.after_fields(ctx);
    }
    fn after_model(&self, model: &T, ctx: &mut ValidationContext) {
        model.after_model(ctx);
    }
    fn computed_fields(
        &self,
        model: &T,
        out: &mut Map<String, Value>,
        opts: &DumpOptions,
    ) -> Result<(), DumpError> {
        model.computed_fields(out, opts)
    }
    fn computed_schema(&self, properties: &mut Map<String, Value>, registry: &mut SchemaRegistry) {
        T::computed_schema(properties, registry);
    }
    fn serialize_field(&self, model: &T, field: &str, value: Value) -> Result<Value, DumpError> {
        model.serialize_field(field, value)
    }
    fn serialize_model(&self, model: &T, value: Value) -> Result<Value, DumpError> {
        model.serialize_model(value)
    }
}

#[cfg(test)]
// Autoref specialisation needs the explicit `&&`; generated code allows this too.
#[allow(clippy::needless_borrow)]
mod tests {
    use super::*;
    use serde_json::json;

    struct WithHooks;
    impl ModelHooks for WithHooks {
        fn after_model(&self, ctx: &mut ValidationContext) {
            ctx.error("custom", "from hook");
        }
        fn serialize_model(&self, _value: Value) -> Result<Value, DumpError> {
            Ok(json!("hooked"))
        }
    }

    struct WithoutHooks;

    #[test]
    fn probe_selects_hooks_when_implemented() {
        let mut ctx = ValidationContext::new();
        (&&Probe::<WithHooks>::new()).after_model(&WithHooks, &mut ctx);
        assert_eq!(ctx.error_count(), 1);
        let out = (&&Probe::<WithHooks>::new())
            .serialize_model(&WithHooks, json!(1))
            .unwrap();
        assert_eq!(out, json!("hooked"));
    }

    #[test]
    fn probe_falls_back_to_no_op() {
        let mut ctx = ValidationContext::new();
        (&&Probe::<WithoutHooks>::new()).after_model(&WithoutHooks, &mut ctx);
        assert_eq!(ctx.error_count(), 0);
        let out = (&&Probe::<WithoutHooks>::new())
            .serialize_model(&WithoutHooks, json!(1))
            .unwrap();
        assert_eq!(out, json!(1));
    }
}
