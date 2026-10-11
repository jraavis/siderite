//! `siderite generate route NAME`: one route module.
//!
//! The module has a documented `GET` handler with an explicit operation
//! ID (`NAME_index`, unique as long as the module name is), a documented
//! response type and a `routes()` function. It is written as a new file
//! only; registering it is a two-line edit that siderite prints instead of
//! rewriting the app's code.

use super::ident::{pascal, snake_name};
use crate::error::CliError;
use std::path::Path;

/// Parsed `generate route` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteOptions {
    /// Module name, snake_case.
    pub name: String,
    /// URL path of the handler.
    pub path: String,
}

const USAGE: &str = "usage: generate route NAME [--path /URL] [--dry-run]";

/// Parse the words after `generate route`.
///
/// Errors:
///   A usage error for a bad name or path.
pub(crate) fn parse(words: &[String]) -> Result<RouteOptions, CliError> {
    let mut name = None;
    let mut path = None;
    let mut args = words.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--path" => path = args.next().cloned(),
            flag if flag.starts_with("--path=") => {
                path = Some(flag["--path=".len()..].to_owned());
            }
            flag if flag.starts_with('-') => {
                let msg = format!("unexpected flag `{flag}`; {USAGE}");
                return Err(CliError::usage(msg));
            }
            word if name.is_none() => name = Some(word.to_owned()),
            _ => return Err(CliError::usage(USAGE)),
        }
    }
    let name = name.ok_or_else(|| CliError::usage(USAGE))?;
    snake_name("route module", &name)?;
    let path = path.unwrap_or_else(|| format!("/{name}"));
    url_path(&path)?;
    Ok(RouteOptions { name, path })
}

/// A literal URL path: `/`, then segments of letters, digits, `-`, `_`
/// and `.`. Parameters (`{id}`) need extractors the template does not
/// write, so they are left to the user.
fn url_path(path: &str) -> Result<(), CliError> {
    let safe = |c: char| c.is_ascii_alphanumeric() || "/-_.".contains(c);
    let literal = path.chars().all(safe) && !path.contains("//");
    if path.starts_with('/') && literal {
        return Ok(());
    }
    Err(CliError::usage(format!(
        "--path `{path}` must start with `/` and use only letters, digits, \
         `-`, `_`, `.` and `/` (add parameters by hand)"
    )))
}

/// Where the module is written, relative to the package.
pub(crate) fn file(options: &RouteOptions) -> String {
    format!("src/routes/{}.rs", options.name)
}

/// The module's source.
pub(crate) fn render(options: &RouteOptions) -> String {
    let RouteOptions { name, path } = options;
    let ty = format!("{}Info", pascal(name));
    format!(
        "\
//! The `{path}` routes.

use siderite::prelude::*;

/// What `GET {path}` returns.
#[derive(Debug, Serialize, Schema)]
pub struct {ty} {{
    /// What this resource is.
    pub message: String,
}}

/// Describe the `{name}` resource.
#[get(\"{path}\", tag = \"{name}\", operation_id = \"{name}_index\")]
pub async fn {name}_index() -> Json<{ty}> {{
    Json({ty} {{
        message: \"{name}\".to_owned(),
    }})
}}

/// The routes of this module, for `App::routes`.
pub fn routes() -> Vec<Route> {{
    routes![{name}_index].into_iter().collect()
}}
"
    )
}

/// The registration steps, which siderite leaves to the user.
pub(crate) fn instructions(dir: &Path, options: &RouteOptions) -> String {
    let name = &options.name;
    let parent = ["src/routes.rs", "src/routes/mod.rs"]
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
            let module = format!("pub mod routes {{ pub mod {name}; }}");
            format!("add `{module}` to {root}")
        }
    };
    format!(
        "Register it (siderite does not edit your code):\n  1. {declare}\n  \
         2. add `.routes(routes::{name}::routes())` where the app is built\n\
         Then run `siderite check`.\n"
    )
}
