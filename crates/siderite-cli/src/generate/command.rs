//! `siderite generate KIND NAME ...`: write new source files into the
//! selected package through the write plan (never overwriting), with
//! `--dry-run` showing the exact files first.

use super::{Plan, model, preview, resource, route, rustfmt, summary};
use crate::error::CliError;
use crate::target::{self, ProjectArgs};
use std::path::Path;

const USAGE: &str = "usage: generate route NAME [--path /URL] \
     [--dry-run]\n       \
     generate model NAME [FIELD:TYPE[?]]... [--table TABLE] [--dry-run]\n  \
          generate resource NAME [FIELD:TYPE[?]]... [--table TABLE]";

const NEEDS_PACKAGE: &str = "generate needs a Cargo package; cd into one";

type Code = Result<u8, CliError>;
type Generated = Result<Output, CliError>;

/// What one generator produces.
struct Output {
    /// Files to create, relative to the package.
    files: Vec<(String, String)>,
    /// What the user does next.
    next: String,
}

/// `siderite generate` from `cwd`.
///
/// Errors:
///   Usage errors, a conflict with an existing file, or a write error.
pub(crate) fn run(raw: &[String], cwd: &Path, project: &ProjectArgs) -> Code {
    let from = raw.iter().position(|a| a == "generate");
    let rest = &raw[from.map_or(raw.len(), |i| i + 1)..];
    let dry_run = rest.iter().any(|a| a == "--dry-run");
    let words = rest.iter().filter(|a| *a != "--dry-run");
    let words: Vec<String> = words.cloned().collect();
    let usage = || CliError::usage(USAGE);
    let (kind, words) = words.split_first().ok_or_else(usage)?;
    let found = target::locate("cargo", cwd, project, false)?;
    let found = found.ok_or_else(|| CliError::usage(NEEDS_PACKAGE))?;
    let output = generate(kind, words, &found.dir)?;
    let mut plan = Plan::new(&found.dir);
    for (file, text) in output.files {
        let text = if file.ends_with(".rs") {
            rustfmt(&text)
        } else {
            text
        };
        plan.add(&file, text)?;
    }
    if dry_run {
        print!("{}", preview(&plan.check()?));
        return Ok(0);
    }
    let changes = plan.apply()?;
    print!("{}", summary(&changes));
    print!("{}", output.next);
    Ok(0)
}

fn generate(kind: &str, words: &[String], dir: &Path) -> Generated {
    match kind {
        "route" => {
            let options = route::parse(words)?;
            Ok(Output {
                files: vec![(route::file(&options), route::render(&options))],
                next: route::instructions(dir, &options),
            })
        }
        "model" => {
            let options = model::parse(words)?;
            Ok(Output {
                files: vec![(model::file(&options), model::render(&options))],
                next: model::instructions(dir, &options),
            })
        }
        "resource" => {
            let options = model::parse(words)?;
            let files = vec![
                (model::file(&options), model::render(&options)),
                (resource::file(&options), resource::render(&options)),
            ];
            Ok(Output {
                files,
                next: resource::instructions(dir, &options),
            })
        }
        _ => Err(CliError::usage(USAGE)),
    }
}
