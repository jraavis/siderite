---
title: Validation
description: Pydantic-style pipeline, field constraints, hooks, Dump, and the 422 problem document.
---

siderite validation is modelled on Pydantic v2, built from Rust traits and
derives rather than runtime type inspection.

```rust
use siderite::prelude::*;

#[derive(Deserialize, Validate, Schema)]
#[serde(rename_all = "camelCase")]
#[model_config(extra = "forbid", str_strip_whitespace)]
struct CreateUser {
    #[field(min_length = 2, max_length = 50)]
    user_name: String,
    #[field(email)]
    email: String,
    #[field(ge = 13)]
    age: Option<u8>,
}
```

`Json<CreateUser>` as a handler argument runs the whole pipeline. When the
input is invalid, the handler is not called. The client gets a **422**
problem document that lists every error, not just the first:

```json
{
  "type": "about:blank",
  "title": "Unprocessable Entity",
  "status": 422,
  "detail": "The request could not be processed; see `errors` for details.",
  "errors": [
    {"location": ["body", "userName"], "code": "too_short", "message": "should have at least 2 items/characters"},
    {"location": ["body", "email"], "code": "invalid_email", "message": "invalid email address"},
    {"location": ["body", "role"], "code": "extra_forbidden", "message": "extra inputs are not permitted"}
  ]
}
```

Error locations use the key the client actually sent (an alias if one was
used). Codes follow Pydantic names where one exists (`int_parsing`,
`missing`, `extra_forbidden`).

## Pipeline

1. **Model before-validators** on the raw JSON / query / form value.
2. **Field walk** — aliases, missing keys, extra keys.
3. **Field before-validators**, then **type-driven `prepare`**: each type
   checks, and in lax mode coerces, its own part of the input (integers
   accept `"42"` and `3.0`; `bool` accepts `"yes"`, `"on"`, `1`; strings
   apply the model’s trim and case settings; constrained types check
   bounds). Coercion works through newtypes and aliases. All errors are
   collected before anything is deserialized.
4. **Field constraints** on the raw value (`min_length`, `ge`, …).
5. **Serde deserialize**. A property test checks that when `prepare`
   accepts an input, Serde accepts it too.
6. **Field after-validators**, then **model after-validators**.

Query strings, forms, and path segments count as *text input*:
string-to-number coercion is always allowed for them, even in strict mode.
A key that is repeated in a query string fills a `Vec` field.

## `#[model_config(...)]`

| Key | Meaning |
|---|---|
| `strict` | No lax coercion: `"1"` is not an integer, and `1` is not a bool |
| `extra = "ignore" \| "forbid" \| "allow"` | Unknown keys. Rust structs cannot store extras, so `allow` accepts them and then drops them. `#[serde(deny_unknown_fields)]` implies `forbid`, and `forbid` sets `additionalProperties: false` |
| `populate_by_name` | For aliased fields, the Rust field name is also accepted |
| `str_strip_whitespace`, `str_to_lower`, `str_to_upper` | Applied to every string field of this model |
| `hooks` | Required on **generic** models that have `#[model_hooks]` |

Settings apply only to the model they are declared on. Nested models use
their own settings.

## `#[field(...)]` validation keys

The same attribute is read by `Validate` and `Schema`, so the constraints
you check are the constraints that get documented. Key names always come
from Serde (`rename`, `rename_all`).

| Key | Validation | Schema |
|---|---|---|
| `min_length`, `max_length` | Characters in a string, or items in a list or map | `minLength` / `maxLength`, or `minItems` / `maxItems` |
| `pattern = "regex"` | Whole-value match. The regex is checked when the macro expands | `pattern` |
| `email` | Email format | `format: email` |
| `gt`, `ge`, `lt`, `le` | Numeric bounds | `exclusiveMinimum`, `minimum`, `exclusiveMaximum`, `maximum` |
| `multiple_of` | Divisibility | `multipleOf` |
| `validation_alias = "x"` | Extra accepted input key | — |
| `default = expr`, `default_factory = path` | Field becomes optional; Serde must also default it | Left out of `required` |
| `strict` | Strict mode for this field only | — |
| `exclude` | Left out of `dump` output | — |
| `validator = path` | Reusable after-validator, `fn(&T) -> Result<(), FieldError>` | — |
| `title`, `description`, `examples(...)` | — | Annotations |

The combined Validate + Model + Schema key list is in
[Field attributes](/siderite/reference/field-attributes/).

## Hooks

```rust
#[model_hooks]
impl CreateUser {
    #[field_validator("user_name", mode = "after")]
    fn not_reserved(value: &str) -> Result<(), FieldError> {
        if value == "admin" {
            Err(FieldError::new("reserved", "name is reserved"))
        } else {
            Ok(())
        }
    }

    #[model_validator(mode = "after")]
    fn adult_email(&self) -> Result<(), FieldError> { Ok(()) }

    #[computed_field]
    fn display_name(&self) -> String { format!("@{}", self.user_name) }
}
```

Order:

1. Model before-validators.
2. For each field, in declaration order: before-validators, type `prepare`,
   constraints.
3. Serde deserialization.
4. Field after-validators, in declaration order.
5. Model after-validators.

The derives find `#[model_hooks]` on concrete types without being told. A
**generic** model such as `Page<T>` with hooks must declare
`#[model_config(hooks)]`. Field validators and serializers take
`(value: &FieldTy)`. A reusable `#[field(validator = path)]` runs right
after the field’s own checks, before the `#[model_hooks]` validators.

## Serialization: `Dump`

`Json<T>` responses serialize through `Dump`, not directly with Serde.

- `#[derive(Schema)]` implements `Dump` for any type that also derives
  `Serialize`. Input-only types still compile. `#[schema(no_dump)]` opts
  out.
- Computed fields, field serializers, and model serializers are included,
  including those of nested models inside `Vec`, `Option`, or maps.
- `JsonDump(value, DumpOptions::new().exclude_none())` controls output per
  response. Options: `exclude_none`, `exclude_defaults`,
  `include(FieldSet)`, `exclude(FieldSet)`.
- A hand-written type opts in with `impl Dump for T {}`, which uses plain
  Serde output.

Computed fields appear in OpenAPI as `readOnly` properties.

`exclude_unset` is not available: Serde does not record which fields were
explicitly set.

Ordinary derived models and lists now write response JSON directly without
building an intermediate JSON tree. The encoded body remains buffered until
serialization succeeds. Computed fields, serializer hooks, and
explicit dump options retain their existing behavior. Hand-written `Dump`
implementations remain compatible: the default `serialize_dump` method calls
`dump`, so custom output is never silently replaced by plain Serde output.
`DumpSerialize(&value, &options)` in `validation::dump` exposes the same
serialization path for other Serde consumers.

## Constrained types

| Type | Accepts |
|---|---|
| `ConstrainedString<MIN, MAX>` | String with MIN..=MAX characters |
| `Email` | Email-format string |
| `SecretString` | Redacted in `Debug`, `Display`, and serialized output |
| `BoundedI64<MIN, MAX>` | Integer in MIN..=MAX |
| `PositiveInt`, `NonNegativeInt`, `NegativeInt` | `> 0`, `>= 0`, `< 0` |
| `Url`, `HttpUrl` | `url` crate; `HttpUrl` only `http` / `https` |
| `IpAddress`, `Ipv4Address`, `Ipv6Address` | IP address strings |
| `Uuid` | Hyphenated or simple UUID, any version |
| `Decimal<MAX_DIGITS, DECIMAL_PLACES>` | Exact decimals. Strict mode accepts only strings |
| `BoundedFloat<B: FloatBounds>` | Finite floats (`UnitInterval`, `NonNegative`, `Positive`, or your own) |
| `ConstrainedVec<T, MIN, MAX>` | List with MIN..=MAX items |

`#[field(max_digits, decimal_places)]` on a plain field only documents the
limits (as `x-` schema extensions). Use the `Decimal` type to enforce them.

## Hand-written implementations

`impl Validate for T {}` opts a type in without checks. For text input
(query strings, forms), implement `prepare` as well; without it,
`?flag=true` remains a string. `siderite::validation::model::{prepare_object,
FieldSpec, check}` are the building blocks the derive uses.

## See also

- [Pydantic v2 mapping](/siderite/reference/pydantic/)
- [Errors](/siderite/guides/http/errors/)
- [OpenAPI 3.1](/siderite/guides/http/openapi/)
