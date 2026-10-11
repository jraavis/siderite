//! Pydantic v2-inspired validation, serialization options and JSON Schema
//! metadata for siderite.
//!
//! - [`Validate`]: type-driven `prepare` (coercion and checks on raw input,
//!   every error collected) plus `validate` (constraints and validators on the
//!   typed value).
//! - [`parse_value`] / [`parse_json`]: the full pipeline, like `model_validate`.
//! - [`ValidationContext`] / [`ModelConfig`]: locations, strictness, string
//!   transforms, extra-key policy and user data.
//! - [`ModelHooks`]: field/model validators, computed fields, serializers.
//! - [`Dump`] / [`DumpOptions`]: `model_dump`-style output options.
//! - [`Schema`]: JSON Schema / OpenAPI 3.1 metadata.
//! - [`Change`]: a partial-update field, absent or set (`null` included).
//! - [`rules`] and constrained [`types`].
#![forbid(unsafe_code)]

pub mod change;
pub mod context;
pub mod dump;
pub mod error;
pub mod hooks;
pub mod model;
pub mod pipeline;
pub mod rules;
pub mod schema;
pub mod types;
pub mod validate;

pub use change::Change;
pub use context::{Extra, InputKind, ModelConfig, ValidationContext};
pub use dump::{Dump, DumpError, DumpOptions, FieldSet};
pub use error::{FieldError, LocationItem, ValidationError, ValidationResult};
pub use hooks::ModelHooks;
pub use pipeline::{parse_json, parse_value, text_pairs_to_value, validate};
pub use rules::{
    decimal_digits, email, ge, gt, ip, le, lt, max_length, min_length, multiple_of, pattern, url,
    uuid,
};
pub use schema::{Schema, SchemaConflict, SchemaObject, SchemaRegistry, schema_for};
pub use types::{
    BoundedFloat, BoundedI64, ConstrainedString, ConstrainedVec, Decimal, Email, FloatBounds,
    HttpUrl, IpAddress, Ipv4Address, Ipv6Address, NegativeInt, NonNegative, NonNegativeInt,
    Positive, PositiveInt, SecretString, UnboundedDecimal, UnitInterval, Url, Uuid,
};
pub use validate::Validate;
