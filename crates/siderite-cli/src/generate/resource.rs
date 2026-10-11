//! `siderite generate resource NAME [FIELD:TYPE[?]]... [--table TABLE]`:
//! a model and its CRUD routes.
//!
//! It writes the same model file as `generate model` (an identical file
//! is left unchanged) and `src/routes/NAME.rs`:
//!
//! - `GET /TABLE?page=N&per_page=M`: one page of rows ordered by `id`
//!   (at most 100 per page, 20 by default), always two queries: a count
//!   and one fetch.
//! - `POST /TABLE`: create; 201 with the stored row.
//! - `GET /TABLE/{id}`: fetch; 404 when there is no such row.
//! - `PUT /TABLE/{id}`: replace every field (the creation body); 200.
//! - `PATCH /TABLE/{id}`: change the fields present in the body; an
//!   absent field keeps its value, `null` clears a nullable field and is
//!   rejected for any other; 200.
//! - `DELETE /TABLE/{id}`: 204, or 404 when there is no such row.
//!
//! The primary key never changes: bodies reject unknown fields, `id`
//! included, with 422, like any body that fails validation. Writes go to
//! the router's write database, reads to its read database
//! (`State<Databases>`, which `AppCli` registers). Updates are
//! read-modify-write, so concurrent writers to one row: last write wins.
//! The handlers are unauthenticated examples, and no migration is run.

use super::ident::pascal;
use super::model::{FieldSpec, ModelOptions};
use std::fmt::Write;
use std::path::Path;

/// Where the routes are written, relative to the package.
pub(crate) fn file(options: &ModelOptions) -> String {
    format!("src/routes/{}.rs", options.name)
}

const NOT_EMPTY: &str = "#[field(min_length = 1)]";

/// The creation body's fields: required strings must not be empty;
/// nullable fields may be left out.
fn body_fields(fields: &[FieldSpec]) -> String {
    let mut out = String::new();
    for f in fields {
        let (doc, attr) = match (f.nullable, f.ty) {
            (true, _) => ("; may be omitted or `null`", "#[serde(default)]"),
            (false, "String") => ("; must not be empty", NOT_EMPTY),
            (false, _) => ("", ""),
        };
        let _ = writeln!(out, "    /// `{}`{doc}.", f.name);
        if !attr.is_empty() {
            let _ = writeln!(out, "    {attr}");
        }
        let _ = writeln!(out, "    pub {}: {},", f.name, f.rust_type());
    }
    out
}

/// The patch body's fields: each is `Change::Unchanged` when absent.
fn patch_fields(fields: &[FieldSpec]) -> String {
    let mut out = String::new();
    for f in fields {
        let doc = if f.nullable {
            "; `null` clears it"
        } else {
            "; `null` is rejected"
        };
        let _ = writeln!(out, "    /// New `{}`, when present{doc}.", f.name);
        if !f.nullable && f.ty == "String" {
            let _ = writeln!(out, "    {NOT_EMPTY}");
        }
        let _ = writeln!(out, "    #[serde(default)]");
        let (name, ty) = (&f.name, f.rust_type());
        let _ = writeln!(out, "    pub {name}: Change<{ty}>,");
    }
    out
}

fn out_fields(fields: &[FieldSpec]) -> String {
    let mut out = String::new();
    for f in fields {
        let _ = writeln!(out, "    /// `{}`.", f.name);
        let _ = writeln!(out, "    pub {}: {},", f.name, f.rust_type());
    }
    out
}

fn names(fields: &[FieldSpec], indent: &str) -> String {
    let mut out = String::new();
    for f in fields {
        let _ = writeln!(out, "{indent}{},", f.name);
    }
    out
}

/// `body.x.apply(&mut row.x);` for every field.
fn apply_patch(fields: &[FieldSpec]) -> String {
    let mut out = String::new();
    for f in fields {
        let n = &f.name;
        let _ = writeln!(out, "    body.{n}.apply(&mut row.{n});");
    }
    out
}

/// The route attribute's tag and operation ID.
fn ids(operation: &str, name: &str) -> String {
    format!("tag = \"{name}\", operation_id = \"{operation}_{name}\"")
}

/// Generated text for one resource.
struct Parts<'a> {
    name: &'a str,
    table: &'a str,
    ty: String,
    fields: &'a [FieldSpec],
}

impl Parts<'_> {
    fn types(&self) -> String {
        let Self { table, ty, .. } = self;
        let body = body_fields(self.fields);
        let patch = patch_fields(self.fields);
        let out = out_fields(self.fields);
        let deep = names(self.fields, "            ");
        format!(
            "\
/// Body of `POST /{table}` and `PUT /{table}/{{id}}`; invalid input is
/// answered with 422.
#[derive(Debug, Deserialize, Validate, Schema)]
#[serde(deny_unknown_fields)]
pub struct New{ty} {{
{body}}}

/// Body of `PATCH /{table}/{{id}}`: only the fields present change.
#[derive(Debug, Deserialize, Validate, Schema)]
#[serde(deny_unknown_fields)]
pub struct {ty}Patch {{
{patch}}}

/// A `{ty}` as returned to clients.
#[derive(Debug, Serialize, Schema)]
pub struct {ty}Out {{
    /// Database key.
    pub id: i64,
{out}}}

impl From<{ty}> for {ty}Out {{
    fn from(row: {ty}) -> Self {{
        let {ty} {{
            id,
{deep}        }} = row;
        Self {{
            id,
{deep}        }}
    }}
}}
"
        )
    }

    fn create_get(&self) -> String {
        let Self {
            name, table, ty, ..
        } = self;
        let shallow = names(self.fields, "        ");
        let (create, get) = (ids("create", name), ids("get", name));
        let list = ids("list", name);
        format!(
            "
/// List `{ty}` rows by `id`, one page at a time: a count and one fetch,
/// whatever the page size. Rows written between the two can make
/// `total` disagree with the items.
#[get(\"/{table}\", {list})]
pub async fn list_{name}(
    State(dbs): State<Databases>,
    Query(params): Query<PageParams>,
) -> ApiResult<Json<Page<{ty}Out>>> {{
    let rows = dbs.objects::<{ty}>()?.order_by([{ty}::id.asc()]);
    Ok(Json(params.fetch(rows).await?.map({ty}Out::from)))
}}

/// Create a `{ty}`; the response holds the stored row.
#[post(\"/{table}\", status = 201, {create})]
pub async fn create_{name}(
    State(dbs): State<Databases>,
    Json(body): Json<New{ty}>,
) -> ApiResult<Json<{ty}Out>> {{
    let New{ty} {{
{shallow}    }} = body;
    let mut row = {ty} {{
        id: 0,
{shallow}    }};
    row.save(dbs.for_write::<{ty}>()?).await?;
    Ok(Json(row.into()))
}}

/// Fetch one `{ty}`; 404 when there is none with this `id`.
#[get(\"/{table}/{{id}}\", {get})]
pub async fn get_{name}(
    State(dbs): State<Databases>,
    Path(id): Path<i64>,
) -> ApiResult<Json<{ty}Out>> {{
    let found = dbs.objects::<{ty}>()?.get({ty}::id.eq(id)).await?;
    Ok(Json(found.into()))
}}
"
        )
    }

    fn update_delete(&self) -> String {
        let Self {
            name, table, ty, ..
        } = self;
        let shallow = names(self.fields, "        ");
        let apply = apply_patch(self.fields);
        let (replace, update) = (ids("replace", name), ids("update", name));
        let delete = ids("delete", name);
        format!(
            "
/// Replace every field of a `{ty}`; 404 when there is none with this
/// `id`. The `id` itself never changes.
#[put(\"/{table}/{{id}}\", {replace})]
pub async fn replace_{name}(
    State(dbs): State<Databases>,
    Path(id): Path<i64>,
    Json(body): Json<New{ty}>,
) -> ApiResult<Json<{ty}Out>> {{
    let db = dbs.for_write::<{ty}>()?;
    let found = {ty}::objects(db).get({ty}::id.eq(id)).await?;
    let New{ty} {{
{shallow}    }} = body;
    let mut row = {ty} {{
        id: found.id,
{shallow}    }};
    row.save(db).await?;
    Ok(Json(row.into()))
}}

/// Change the fields present in the body; 404 when there is no `{ty}`
/// with this `id`.
#[patch(\"/{table}/{{id}}\", {update})]
pub async fn update_{name}(
    State(dbs): State<Databases>,
    Path(id): Path<i64>,
    Json(body): Json<{ty}Patch>,
) -> ApiResult<Json<{ty}Out>> {{
    let db = dbs.for_write::<{ty}>()?;
    let mut row = {ty}::objects(db).get({ty}::id.eq(id)).await?;
{apply}    row.save(db).await?;
    Ok(Json(row.into()))
}}

/// Delete a `{ty}`; 404 when there is none with this `id`.
#[delete(\"/{table}/{{id}}\", status = 204, {delete})]
pub async fn delete_{name}(
    State(dbs): State<Databases>,
    Path(id): Path<i64>,
) -> ApiResult<NoContent> {{
    let db = dbs.for_write::<{ty}>()?;
    let rows = {ty}::objects(db).filter({ty}::id.eq(id));
    if rows.delete().await? == 0 {{
        return Err(ApiError::not_found(\"Resource not found.\"));
    }}
    Ok(NoContent)
}}
"
        )
    }
}

/// The routes module's source.
pub(crate) fn render(options: &ModelOptions) -> String {
    let parts = Parts {
        name: &options.name,
        table: &options.table,
        ty: pascal(&options.name),
        fields: &options.fields,
    };
    let Parts {
        name, table, ty, ..
    } = &parts;
    let handlers = ["list", "create", "get", "replace", "update", "delete"]
        .map(|op| format!("{op}_{name}"))
        .join(", ");
    format!(
        "\
//! `/{table}`: list, create, fetch, replace, update and delete `{ty}` rows.
//!
//! Example handlers without authentication: add your own checks before
//! exposing them.

use crate::models::{name}::{ty};
use siderite::orm::Databases;
use siderite::prelude::*;
use siderite::{{Page, PageParams}};

{types}{create_get}{update_delete}
/// The routes of this module, for `App::routes`.
pub fn routes() -> Vec<Route> {{
    routes![{handlers}].into_iter().collect()
}}
",
        types = parts.types(),
        create_get = parts.create_get(),
        update_delete = parts.update_delete(),
    )
}

const REGISTER: &str = "Register them (siderite does not edit your code):\n";

/// The registration steps, which siderite leaves to the user.
pub(crate) fn instructions(dir: &Path, options: &ModelOptions) -> String {
    let name = &options.name;
    let model = super::model::instructions(dir, options);
    let steps = model.lines().skip(1).take(2);
    let mut out = String::from(REGISTER);
    for line in steps {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(
        out,
        "  3. declare `routes::{name}` like `models::{name}` and add \
         `.routes(routes::{name}::routes())` where the app is built\n\
         Then run `siderite makemigrations` and `siderite migrate`."
    );
    out
}
