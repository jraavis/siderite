//! `siderite generate model NAME [FIELD:TYPE]... [--table TABLE]`: one
//! model.
//!
//! The model has an auto-assigned `i64` primary key named `id` and the
//! given scalar fields. The type set is deliberately small: `i32`, `i64`,
//! `f64`, `bool` and `String`, each nullable with a `?` suffix
//! (`Option<T>`). Those map to columns on every SQL backend and need no
//! extra dependency; dates, UUIDs and decimals are left to hand-written
//! code until the generator covers their dependencies. Registering the
//! model is printed, not written.

use super::ident::{pascal, snake_name};
use crate::error::CliError;
use std::path::Path;

/// Rust types a generated field may have.
const TYPES: &[&str] = &["i32", "i64", "f64", "bool", "String"];

/// Field names the `Model` traits use as associated items; a field
/// constant of that name would hide them.
const RESERVED: &[&str] = &[
    "id",
    "objects",
    "pk",
    "set_pk",
    "from_row",
    "to_values",
    "is_unsaved",
    "save",
    "delete",
    "refresh",
];

/// One generated field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldSpec {
    /// Field and column name.
    pub name: String,
    /// One of [`TYPES`].
    pub ty: &'static str,
    /// `Option<T>`, a nullable column.
    pub nullable: bool,
}

impl FieldSpec {
    /// The Rust type of the field.
    pub(crate) fn rust_type(&self) -> String {
        if self.nullable {
            format!("Option<{}>", self.ty)
        } else {
            self.ty.to_owned()
        }
    }
}

/// Parsed `generate model` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelOptions {
    /// Module name, snake_case; the type is its PascalCase form.
    pub name: String,
    /// Table name.
    pub table: String,
    /// Fields after `id`, in order.
    pub fields: Vec<FieldSpec>,
}

const USAGE: &str = "usage: generate model NAME [FIELD:TYPE[?]]... \
     [--table TABLE] [--dry-run]\n       TYPE is i32, i64, f64, bool or \
     String; `?` makes it nullable";

/// Parse the words after `generate model`.
///
/// Errors:
///   A usage error for a bad name, table, field or type.
pub(crate) fn parse(words: &[String]) -> Result<ModelOptions, CliError> {
    let mut name = None;
    let mut table = None;
    let mut fields: Vec<FieldSpec> = Vec::new();
    let mut args = words.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--table" => table = args.next().cloned(),
            flag if flag.starts_with("--table=") => {
                table = Some(flag["--table=".len()..].to_owned());
            }
            flag if flag.starts_with('-') => {
                let msg = format!("unexpected flag `{flag}`; {USAGE}");
                return Err(CliError::usage(msg));
            }
            word if name.is_none() => name = Some(word.to_owned()),
            spec => {
                let field = field(spec)?;
                if fields.iter().any(|f| f.name == field.name) {
                    let msg = format!("field `{}` is given twice", field.name);
                    return Err(CliError::usage(msg));
                }
                fields.push(field);
            }
        }
    }
    let name = name.ok_or_else(|| CliError::usage(USAGE))?;
    snake_name("model", &name)?;
    let table = table.unwrap_or_else(|| name.clone());
    snake_name("table", &table)?;
    Ok(ModelOptions {
        name,
        table,
        fields,
    })
}

/// `name:Type` or `name:Type?`.
fn field(spec: &str) -> Result<FieldSpec, CliError> {
    let Some((name, ty)) = spec.split_once(':') else {
        let msg = format!("`{spec}` is not FIELD:TYPE; {USAGE}");
        return Err(CliError::usage(msg));
    };
    snake_name("field", name)?;
    if RESERVED.contains(&name) {
        return Err(CliError::usage(format!(
            "field name `{name}` is reserved (`id` is the primary key; the \
             others are model methods)"
        )));
    }
    let (ty, nullable) = match ty.strip_suffix('?') {
        Some(inner) => (inner, true),
        None => (ty, false),
    };
    let Some(ty) = TYPES.iter().copied().find(|t| *t == ty) else {
        return Err(CliError::usage(format!(
            "type `{ty}` is not supported by the generator; use {}",
            TYPES.join(", ")
        )));
    };
    Ok(FieldSpec {
        name: name.to_owned(),
        ty,
        nullable,
    })
}

/// Where the model is written, relative to the package.
pub(crate) fn file(options: &ModelOptions) -> String {
    format!("src/models/{}.rs", options.name)
}

/// The model's source.
pub(crate) fn render(options: &ModelOptions) -> String {
    let ModelOptions { name, table, .. } = options;
    let ty = pascal(name);
    let mut fields = String::new();
    for f in &options.fields {
        let rust = f.rust_type();
        let note = if f.nullable { "; `None` is `NULL`" } else { "" };
        fields.push_str(&format!(
            "    /// The `{}` column{note}.\n    pub {}: {rust},\n",
            f.name, f.name
        ));
    }
    format!(
        "\
//! The `{ty}` model (table `{table}`).

use siderite::prelude::*;

/// A row of `{table}`.
#[derive(Debug, Clone, Model)]
#[model(table = \"{table}\", ordering = [\"id\"])]
pub struct {ty} {{
    /// Database key, assigned on insert.
    #[field(primary_key, auto)]
    pub id: i64,
{fields}}}
"
    )
}

/// The registration steps, which siderite leaves to the user.
pub(crate) fn instructions(dir: &Path, options: &ModelOptions) -> String {
    let name = &options.name;
    let ty = pascal(name);
    let parent = ["src/models.rs", "src/models/mod.rs"]
        .into_iter()
        .find(|f| dir.join(f).is_file());
    let root = if dir.join("src/lib.rs").is_file() {
        "src/lib.rs"
    } else {
        "src/main.rs"
    };
    let declare = match parent {
        Some(file) => format!("add `pub mod {name};` to {file}"),
        None => {
            let module = format!("pub mod models {{ pub mod {name}; }}");
            format!("add `{module}` to {root}")
        }
    };
    let meta = format!("models::{name}::{ty}::META");
    format!(
        "Register it (siderite does not edit your code):\n  1. {declare}\n  \
         2. add `{meta}` to `AppCli::models(&[...])`\n\
         Then run `siderite makemigrations` and review the migration.\n"
    )
}
