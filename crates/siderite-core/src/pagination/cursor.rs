//! Cursor pagination over HTTP: [`CursorParams`] reads and checks
//! `?after=TOKEN&limit=N` (or `?before=TOKEN&limit=N`), and
//! [`CursorPage`] is the response body.
//!
//! [`CursorPage`] is built from the ORM's
//! [`orm::CursorPage`](siderite_orm::CursorPage): one statement per page
//! and no `total`. A token the server rejects (another list's, a changed
//! filter, a mangled one) answers `400` with the problem `code`
//! `cursor.E001` to `cursor.E004`; clients restart from the first page.
//! The client repeats its query parameters with each request, and the
//! handler rebuilds the same queryset, because the token does not carry
//! the filters.

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Number, Value, json};
use siderite_orm::{Model, OrmError, QuerySet};
use siderite_validation::dump::DumpSerialize;
use siderite_validation::model::{Constraint, check};
use siderite_validation::{Dump, DumpError, DumpOptions, Schema};
use siderite_validation::{SchemaObject, SchemaRegistry};
use siderite_validation::{Validate, ValidationContext};

/// The result of [`CursorParams::fetch`].
pub type Fetched<M> = Result<CursorPage<M>, OrmError>;

/// `?after=TOKEN` or `?before=TOKEN`, and `?limit=N` from 1 to `MAX`,
/// `DEFAULT` when absent.
///
/// Without a token the first page is returned. Both tokens together, an
/// out-of-range or non-numeric limit are rejected with 422 before the
/// handler runs, and the documented query parameters state the bounds.
/// Use it as `Query<CursorParams>` (at most 100, default 20) or choose
/// the bounds: `Query<CursorParams<500, 50>>`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct CursorParams<const MAX: u64 = 100, const DEFAULT: u64 = 20> {
    /// The `next` cursor of the previous page: continue after it.
    #[serde(default)]
    pub after: Option<String>,
    /// The `previous` cursor of a page: the page before it.
    #[serde(default)]
    pub before: Option<String>,
    /// Items per page.
    #[serde(default)]
    pub limit: Option<u64>,
}

impl<const MAX: u64, const DEFAULT: u64> CursorParams<MAX, DEFAULT> {
    /// Checked once per instantiation, at compile time.
    const BOUNDS: () = assert!(
        1 <= DEFAULT && DEFAULT <= MAX,
        "CursorParams needs 1 <= DEFAULT <= MAX"
    );

    /// The requested page size, `DEFAULT` when absent.
    pub fn limit(&self) -> u64 {
        let () = Self::BOUNDS;
        self.limit.unwrap_or(DEFAULT)
    }

    /// Fetch this page of `queryset` (one statement, no count).
    ///
    /// # Errors
    /// [`OrmError::Cursor`] for a token the queryset rejects (answered
    /// with 400 by `ApiError`), capability or backend errors.
    pub async fn fetch<M: Model>(&self, queryset: QuerySet<M>) -> Fetched<M> {
        let mut queryset = queryset;
        if let Some(token) = &self.after {
            queryset = queryset.after(token);
        }
        if let Some(token) = &self.before {
            queryset = queryset.before(token);
        }
        let page = queryset.cursor_page(self.limit()).await?;
        Ok(page.into())
    }
}

impl<const MAX: u64, const D: u64> Validate for CursorParams<MAX, D> {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        let Value::Object(fields) = input else {
            return;
        };
        for key in ["after", "before"] {
            if let Some(value) = fields.get_mut(key) {
                ctx.at(key, |ctx| Option::<String>::prepare(value, ctx));
            }
        }
        let given = |key| fields.get(key).is_some_and(|v| !v.is_null());
        if given("after") && given("before") {
            ctx.at("before", |ctx| {
                ctx.error("exclusive", "cannot be combined with `after`");
            });
        }
        let Some(value) = fields.get_mut("limit") else {
            return;
        };
        ctx.at("limit", |ctx| {
            let before = ctx.error_count();
            Option::<u64>::prepare(value, ctx);
            if ctx.error_count() > before {
                return;
            }
            check(value, Constraint::Ge(&Number::from(1_u64)), ctx);
            check(value, Constraint::Le(&Number::from(MAX)), ctx);
        });
    }
}

impl<const MAX: u64, const D: u64> Schema for CursorParams<MAX, D> {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        let properties = json!({
            "after": {
                "type": "string",
                "description": "The `next` cursor of the previous page. \
                    Not combined with `before`.",
            },
            "before": {
                "type": "string",
                "description": "The `previous` cursor of a page, to read \
                    the page before it. Not combined with `after`.",
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": MAX,
                "default": D,
                "description": "Items per page.",
            },
        });
        SchemaObject::of_type("object").with("properties", properties)
    }
}

/// One page of `T` as a response body, with the cursors to its
/// neighbours and no total.
///
/// Its schema is inlined rather than named, so pages of different item
/// types never collide in `components.schemas`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CursorPage<T> {
    /// The items of this page.
    pub items: Vec<T>,
    /// Cursor for the page after this one; `null` on the last page.
    pub next: Option<String>,
    /// Cursor for the page before this one; `null` on the first page.
    pub previous: Option<String>,
}

impl<T> CursorPage<T> {
    /// The same page with each item converted by `f`.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> CursorPage<U> {
        CursorPage {
            items: self.items.into_iter().map(f).collect(),
            next: self.next,
            previous: self.previous,
        }
    }

    /// The same page with each item converted by `f`, stopping at the
    /// first error.
    ///
    /// # Errors
    /// The first error `f` returns.
    pub fn try_map<U, E, F>(self, f: F) -> Result<CursorPage<U>, E>
    where
        F: FnMut(T) -> Result<U, E>,
    {
        let items = self.items.into_iter().map(f).collect::<Result<_, _>>()?;
        Ok(CursorPage {
            items,
            next: self.next,
            previous: self.previous,
        })
    }
}

impl<M> From<siderite_orm::CursorPage<M>> for CursorPage<M> {
    fn from(page: siderite_orm::CursorPage<M>) -> Self {
        Self {
            items: page.items,
            next: page.next,
            previous: page.previous,
        }
    }
}

impl<T: Dump> Dump for CursorPage<T> {
    fn serialize_dump<S: Serializer>(
        &self,
        serializer: S,
        opts: &DumpOptions,
    ) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_struct("CursorPage", 3)?;
        out.serialize_field("items", &DumpSerialize(&self.items[..], opts))?;
        out.serialize_field("next", &self.next)?;
        out.serialize_field("previous", &self.previous)?;
        out.end()
    }

    fn dump(&self, opts: &DumpOptions) -> Result<Value, DumpError> {
        Ok(serde_json::to_value(DumpSerialize(self, opts))?)
    }
}

impl<T: Schema + 'static> Schema for CursorPage<T> {
    fn schema(registry: &mut SchemaRegistry) -> SchemaObject {
        let items = registry.subschema::<Vec<T>>().into_value();
        let cursor = registry.subschema::<Option<String>>().into_value();
        let properties = json!({
            "items": items,
            "next": cursor,
            "previous": cursor,
        });
        let required = ["items", "next", "previous"];
        SchemaObject::of_type("object")
            .with("properties", properties)
            .with("required", json!(required))
    }
}
