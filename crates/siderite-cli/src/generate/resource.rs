//! `siderite generate resource NAME [FIELD:TYPE[?]]... [--table TABLE]`:
//! a model and its create/get routes.
//!
//! It writes the same model file as `generate model` (an identical file
//! is left unchanged) and `src/routes/NAME.rs` with a request body type,
//! a response type and two handlers: `POST /TABLE` (201 with the stored
//! row; invalid bodies are 422 before the handler runs) and
//! `GET /TABLE/{id}` (404 when there is no such row). Handlers reach the
//! database through `State<Databases>`, which `AppCli` registers. They
//! are unauthenticated examples, and no migration is run.

use super::ident::pascal;
use super::model::{FieldSpec, ModelOptions};
use std::fmt::Write;
use std::path::Path;

/// Where the routes are written, relative to the package.
pub(crate) fn file(options: &ModelOptions) -> String {
    format!("src/routes/{}.rs", options.name)
}

const NOT_EMPTY: &str = "#[field(min_length = 1)]";

/// The request body's fields: required strings must not be empty;
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

/// The route attribute's tag and operation ID.
fn ids(operation: &str, name: &str) -> String {
    format!("tag = \"{name}\", operation_id = \"{operation}_{name}\"")
}

/// The routes module's source.
pub(crate) fn render(options: &ModelOptions) -> String {
    let ModelOptions {
        name,
        table,
        fields,
    } = options;
    let ty = pascal(name);
    let new = format!("New{ty}");
    let out = format!("{ty}Out");
    let (create_ids, get_ids) = (ids("create", name), ids("get", name));
    let body = body_fields(fields);
    let out_body = out_fields(fields);
    let deep = names(fields, "            ");
    let shallow = names(fields, "        ");
    format!(
        "\
//! `/{table}`: create and fetch `{ty}` rows.
//!
//! Example handlers without authentication: add your own checks before
//! exposing them.

use crate::models::{name}::{ty};
use siderite::orm::Databases;
use siderite::prelude::*;

/// Body of `POST /{table}`; invalid input is answered with 422.
#[derive(Debug, Deserialize, Validate, Schema)]
pub struct {new} {{
{body}}}

/// A `{ty}` as returned to clients.
#[derive(Debug, Serialize, Schema)]
pub struct {out} {{
    /// Database key.
    pub id: i64,
{out_body}}}

impl From<{ty}> for {out} {{
    fn from(row: {ty}) -> Self {{
        let {ty} {{
            id,
{deep}        }} = row;
        Self {{
            id,
{deep}        }}
    }}
}}

/// Create a `{ty}`; the response holds the stored row.
#[post(\"/{table}\", status = 201, {create_ids})]
pub async fn create_{name}(
    State(dbs): State<Databases>,
    Json(body): Json<{new}>,
) -> ApiResult<Json<{out}>> {{
    let {new} {{
{shallow}    }} = body;
    let mut row = {ty} {{
        id: 0,
{shallow}    }};
    row.save(dbs.for_write::<{ty}>()?).await?;
    Ok(Json(row.into()))
}}

/// Fetch one `{ty}`; 404 when there is none with this `id`.
#[get(\"/{table}/{{id}}\", {get_ids})]
pub async fn get_{name}(
    State(dbs): State<Databases>,
    Path(id): Path<i64>,
) -> ApiResult<Json<{out}>> {{
    let found = dbs.objects::<{ty}>()?.get({ty}::id.eq(id)).await?;
    Ok(Json(found.into()))
}}

/// The routes of this module, for `App::routes`.
pub fn routes() -> Vec<Route> {{
    routes![create_{name}, get_{name}].into_iter().collect()
}}
"
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
