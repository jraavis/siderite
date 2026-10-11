//! Page-number pagination over HTTP: [`PageParams`] reads and checks
//! `?page=N&per_page=M`, and [`Page`] is the response body.
//!
//! [`Page`] is built from the ORM's [`orm::Page`](siderite_orm::Page)
//! (`QuerySet::paginate` is unchanged: one `COUNT` and one fetch), so
//! `total` and `items` come from two statements. Without a transaction
//! that pins a snapshot, rows written between them can make `total` and
//! the items disagree, and an unordered queryset can return a row on two
//! pages or on none; order by a unique key for stable pages.

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Number, Value, json};
use siderite_orm::{Model, OrmError, QuerySet};
use siderite_validation::dump::DumpSerialize;
use siderite_validation::model::{Constraint, check};
use siderite_validation::{Dump, DumpError, DumpOptions, Schema};
use siderite_validation::{SchemaObject, SchemaRegistry};
use siderite_validation::{Validate, ValidationContext};

/// The result of [`PageParams::fetch`].
pub type Fetched<M> = Result<Page<M>, OrmError>;

/// `?page=N&per_page=M`: a 1-based page and a page size from 1 to `MAX`,
/// `DEFAULT` when absent.
///
/// Out-of-range or non-numeric values are rejected with 422 before the
/// handler runs, and the documented query parameters state the bounds.
/// Use it as `Query<PageParams>` (at most 100, default 20) or choose the
/// bounds: `Query<PageParams<500, 50>>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub struct PageParams<const MAX: u64 = 100, const DEFAULT: u64 = 20> {
    /// 1-based page number.
    #[serde(default)]
    pub page: Option<u64>,
    /// Items per page.
    #[serde(default)]
    pub per_page: Option<u64>,
}

impl<const MAX: u64, const DEFAULT: u64> PageParams<MAX, DEFAULT> {
    /// Checked once per instantiation, at compile time.
    const BOUNDS: () = assert!(
        1 <= DEFAULT && DEFAULT <= MAX,
        "PageParams needs 1 <= DEFAULT <= MAX"
    );

    /// The requested page, 1 when absent.
    pub fn page(&self) -> u64 {
        self.page.unwrap_or(1)
    }

    /// The requested page size, `DEFAULT` when absent.
    pub fn per_page(&self) -> u64 {
        let () = Self::BOUNDS;
        self.per_page.unwrap_or(DEFAULT)
    }

    /// Fetch this page of `queryset` (one count, one fetch).
    ///
    /// # Errors
    /// Capability or backend errors of the queryset.
    pub async fn fetch<M: Model>(&self, queryset: QuerySet<M>) -> Fetched<M> {
        let page = queryset.paginate(self.page(), self.per_page()).await?;
        Ok(page.into())
    }
}

impl<const MAX: u64, const D: u64> Validate for PageParams<MAX, D> {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        let Value::Object(fields) = input else {
            return;
        };
        let one = Number::from(1_u64);
        let max = Number::from(MAX);
        for (key, upper) in [("page", None), ("per_page", Some(&max))] {
            let Some(value) = fields.get_mut(key) else {
                continue;
            };
            ctx.at(key, |ctx| {
                let before = ctx.error_count();
                Option::<u64>::prepare(value, ctx);
                if ctx.error_count() > before {
                    return;
                }
                check(value, Constraint::Ge(&one), ctx);
                if let Some(upper) = upper {
                    check(value, Constraint::Le(upper), ctx);
                }
            });
        }
    }
}

impl<const MAX: u64, const D: u64> Schema for PageParams<MAX, D> {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        let properties = json!({
            "page": {
                "type": "integer",
                "minimum": 1,
                "default": 1,
                "description": "1-based page number.",
            },
            "per_page": {
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

/// One page of `T` as a response body.
///
/// Its schema is inlined rather than named, so pages of different item
/// types never collide in `components.schemas`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Page<T> {
    /// The items of this page.
    pub items: Vec<T>,
    /// Items over all pages, counted before the page was fetched.
    pub total: u64,
    /// 1-based number of this page.
    pub page: u64,
    /// Requested page size.
    pub per_page: u64,
    /// Number of pages for `total` items.
    pub total_pages: u64,
}

impl<T> Page<T> {
    /// The same page with each item converted by `f`.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Page<U> {
        Page {
            items: self.items.into_iter().map(f).collect(),
            total: self.total,
            page: self.page,
            per_page: self.per_page,
            total_pages: self.total_pages,
        }
    }

    /// The same page with each item converted by `f`, stopping at the
    /// first error.
    ///
    /// # Errors
    /// The first error `f` returns.
    pub fn try_map<U, E, F>(self, f: F) -> Result<Page<U>, E>
    where
        F: FnMut(T) -> Result<U, E>,
    {
        let items = self.items.into_iter().map(f).collect::<Result<_, _>>()?;
        Ok(Page {
            items,
            total: self.total,
            page: self.page,
            per_page: self.per_page,
            total_pages: self.total_pages,
        })
    }
}

impl<M> From<siderite_orm::Page<M>> for Page<M> {
    fn from(page: siderite_orm::Page<M>) -> Self {
        let total_pages = page.total_pages();
        Self {
            items: page.items,
            total: page.total,
            page: page.page,
            per_page: page.per_page,
            total_pages,
        }
    }
}

impl<T: Dump> Dump for Page<T> {
    fn serialize_dump<S: Serializer>(
        &self,
        serializer: S,
        opts: &DumpOptions,
    ) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_struct("Page", 5)?;
        out.serialize_field("items", &DumpSerialize(&self.items[..], opts))?;
        out.serialize_field("total", &self.total)?;
        out.serialize_field("page", &self.page)?;
        out.serialize_field("per_page", &self.per_page)?;
        out.serialize_field("total_pages", &self.total_pages)?;
        out.end()
    }

    fn dump(&self, opts: &DumpOptions) -> Result<Value, DumpError> {
        Ok(serde_json::to_value(DumpSerialize(self, opts))?)
    }
}

impl<T: Schema + 'static> Schema for Page<T> {
    fn schema(registry: &mut SchemaRegistry) -> SchemaObject {
        let items = registry.subschema::<Vec<T>>().into_value();
        let count = registry.subschema::<u64>().into_value();
        let properties = json!({
            "items": items,
            "total": count,
            "page": count,
            "per_page": count,
            "total_pages": count,
        });
        let required = ["items", "total", "page", "per_page", "total_pages"];
        SchemaObject::of_type("object")
            .with("properties", properties)
            .with("required", json!(required))
    }
}

#[cfg(test)]
mod tests;
