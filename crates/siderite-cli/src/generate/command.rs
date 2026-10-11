//! `siderite generate KIND NAME ...`: write new source files into the
//! selected package through the write plan (never overwriting), with
//! `--dry-run` showing the exact files first.

use super::{Plan, ci, model, preview, resource, route, rustfmt, summary, test};
use crate::error::CliError;
use crate::target::{self, ProjectArgs};
use std::path::{Path, PathBuf};

const USAGE: &str = "usage: generate route NAME [--path /URL] \
     [--dry-run]\n       \
     generate model NAME [FIELD:TYPE[?]]... [--table TABLE] [--dry-run]\n  \
          generate resource NAME [FIELD:TYPE[?]]... [--table TABLE]\n  \
          generate test route|resource NAME ...\n       \
          generate ci [--update] [--dry-run]";

const NEEDS_PACKAGE: &str = "generate needs a Cargo package; cd into one";

type Code = Result<u8, CliError>;
type Generated = Result<Output, CliError>;

/// What one generator produces.
struct Output {
    /// Files to create, relative to the package.
    files: Vec<(String, String)>,
    /// What the user does next.
    next: String,
    /// Where the files go, when not the package directory.
    root: Option<PathBuf>,
    /// Whether a file this generator wrote earlier may be replaced.
    owned: bool,
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
    let krate = found.package.replace('-', "_");
    let output = match kind.as_str() {
        "test" => tests(words, &found.dir, &krate)?,
        "ci" => workflow(words, &found.dir)?,
        _ => generate(kind, words, &found.dir)?,
    };
    let root = output.root.as_deref().unwrap_or(&found.dir);
    let mut plan = Plan::new(root);
    for (file, text) in output.files {
        let text = if file.ends_with(".rs") {
            rustfmt(&text)
        } else {
            text
        };
        if output.owned {
            plan.add_owned(&file, text)?;
        } else {
            plan.add(&file, text)?;
        }
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
                root: None,
                owned: false,
            })
        }
        "model" => {
            let options = model::parse(words)?;
            Ok(Output {
                files: vec![(model::file(&options), model::render(&options))],
                next: model::instructions(dir, &options),
                root: None,
                owned: false,
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
                root: None,
                owned: false,
            })
        }
        _ => Err(CliError::usage(USAGE)),
    }
}

/// `generate test`: one test file for the library crate `krate`.
fn tests(words: &[String], dir: &Path, krate: &str) -> Generated {
    let options = test::parse(words)?;
    Ok(Output {
        files: vec![(test::file(&options), test::render(&options, krate))],
        next: test::instructions(dir, &options),
        root: None,
        owned: false,
    })
}

/// `generate ci`: the workflow at the repository root.
fn workflow(words: &[String], dir: &Path) -> Generated {
    let options = ci::parse(words)?;
    let repo = ci::repository(dir);
    let existing = std::fs::read_to_string(repo.join(ci::FILE)).ok();
    ci::may_replace(existing.as_deref(), options.update)?;
    let ws = crate::workspace::load("cargo", dir, None)?;
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.into());
    let package = ws.packages.iter().find(|p| p.canonical_dir() == canonical);
    let package = package.ok_or_else(|| CliError::usage(NEEDS_PACKAGE))?;
    let lock = dir.ancestors().map(|d| d.join("Cargo.lock"));
    let lock = lock.into_iter().find(|p| p.is_file());
    let lock = lock.and_then(|p| std::fs::read_to_string(p).ok());
    let lock = lock.unwrap_or_default();
    let rustc = std::process::Command::new("rustc")
        .arg("--version")
        .current_dir(dir)
        .output();
    let rustc = rustc.map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    let rustc = rustc.unwrap_or_default();
    let rust_version = package.rust_version.as_deref();
    let canonical_repo = repo.canonicalize().unwrap_or_else(|_| repo.clone());
    let rel = canonical
        .strip_prefix(&canonical_repo)
        .unwrap_or(Path::new(""));
    let inputs = ci::Inputs {
        working_dir: rel.to_string_lossy().replace('\\', "/"),
        toolchain: ci::toolchain(dir, rust_version, &rustc),
        install: ci::install(package, &lock, &canonical_repo)?,
        databases: ci::databases(package),
    };
    Ok(Output {
        files: vec![(ci::FILE.to_owned(), ci::render(&inputs))],
        next: ci::instructions(&inputs),
        root: Some(repo),
        owned: options.update,
    })
}
