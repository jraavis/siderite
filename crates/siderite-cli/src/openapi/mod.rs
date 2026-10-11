//! `openapi export [--output PATH] [--force]`: the app's OpenAPI document.
//!
//! The document comes from [`App::openapi`], the same call that serves
//! `/openapi.json`, on an app freshly built by the factory: no database
//! is connected, no server is started and no migration runs. It is the
//! raw OpenAPI 3.1 document (not a report envelope), pretty-printed with
//! a trailing newline. Export does not depend on the HTTP docs settings,
//! so it works when `/openapi.json` is disabled.

use crate::error::CliError;
use crate::fsio;
use crate::report::channel;
use siderite_core::App;
use std::path::{Path, PathBuf};

/// Parsed `openapi export` arguments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Export {
    /// `--output PATH`; stdout when `None`.
    pub output: Option<PathBuf>,
    /// `--force`: replace an existing output file.
    pub force: bool,
}

const USAGE: &str = "usage: openapi export [--output PATH] [--force]";

/// Parse `rest` (`["openapi", "export", ...]`).
///
/// Errors:
///   A usage error for a missing or unknown subcommand or flag.
pub(crate) fn parse(rest: &[String]) -> Result<Export, CliError> {
    match rest.get(1).map(String::as_str) {
        Some("export") => {}
        Some(other) => {
            let msg = format!("unknown openapi command `{other}`; {USAGE}");
            return Err(CliError::usage(msg));
        }
        None => return Err(CliError::usage(USAGE)),
    }
    let mut export = Export::default();
    let mut args = rest[2..].iter();
    while let Some(arg) = args.next() {
        let value = match arg.as_str() {
            "--force" => {
                export.force = true;
                continue;
            }
            "--output" | "-o" => args.next().ok_or_else(|| missing(arg))?,
            other => other
                .strip_prefix("--output=")
                .ok_or_else(|| unexpected(other))?,
        };
        export.output = Some(output_path(value)?);
    }
    if export.force && export.output.is_none() {
        return Err(CliError::usage("--force needs --output"));
    }
    Ok(export)
}

fn missing(flag: &str) -> CliError {
    CliError::usage(format!("{flag} requires a value"))
}

fn unexpected(arg: &str) -> CliError {
    CliError::usage(format!("unexpected argument `{arg}`; {USAGE}"))
}

fn output_path(value: &str) -> Result<PathBuf, CliError> {
    if value.is_empty() {
        return Err(CliError::usage("--output needs a file path"));
    }
    Ok(PathBuf::from(value))
}

/// Whether `raw` asks for a file (`--output`), so stdout carries no
/// document.
pub(crate) fn writes_file(raw: &[String]) -> bool {
    raw.iter()
        .any(|a| a == "--output" || a == "-o" || a.starts_with("--output="))
}

/// Make a relative `--output` absolute against `cwd`, the directory the
/// user ran `siderite` in (the application itself runs in its package).
pub(crate) fn absolute_output(raw: &[String], cwd: &Path) -> Vec<String> {
    let absolute = |value: &str| {
        if value.is_empty() || Path::new(value).is_absolute() {
            value.to_owned()
        } else {
            cwd.join(value).display().to_string()
        }
    };
    let mut out = Vec::with_capacity(raw.len());
    let mut args = raw.iter();
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--output=") {
            out.push(format!("--output={}", absolute(value)));
            continue;
        }
        out.push(arg.clone());
        if (arg == "--output" || arg == "-o")
            && let Some(value) = args.next()
        {
            out.push(absolute(value));
        }
    }
    out
}

/// The OpenAPI document of `app` as pretty JSON with a trailing newline.
///
/// Errors:
///   [`CliError::OpenApi`] when the document cannot be generated (for
///   example duplicate operation ids).
pub(crate) fn render(app: &App) -> Result<String, CliError> {
    let document = app
        .openapi()
        .map_err(|err| CliError::OpenApi(err.to_string()))?;
    let mut text = serde_json::to_string_pretty(&document)
        .map_err(|err| CliError::OpenApi(err.to_string()))?;
    text.push('\n');
    Ok(text)
}

/// Run `export` for `app`: write the document atomically to its output
/// file, or to the report channel (`report_path`, else stdout).
///
/// Errors:
///   [`CliError::OpenApi`] for a document that cannot be generated, a
///   usage error for an existing output without `--force`, or
///   [`CliError::Io`] when writing fails.
pub(crate) fn export(
    app: &App,
    export: &Export,
    report_path: Option<&Path>,
) -> Result<u8, CliError> {
    let text = render(app)?;
    match &export.output {
        Some(path) => {
            fsio::write_atomic(path, text.as_bytes(), export.force)?;
            eprintln!("Wrote the OpenAPI document to {}", path.display());
        }
        None => channel::emit(&text, report_path)?,
    }
    Ok(0)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod unit {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn parses_output_and_force() {
        let parsed = parse(&args(&["openapi", "export"])).unwrap();
        assert_eq!(parsed, Export::default());
        for list in [
            ["openapi", "export", "--output", "a.json"].as_slice(),
            &["openapi", "export", "-o", "a.json"],
            &["openapi", "export", "--output=a.json"],
        ] {
            let parsed = parse(&args(list)).unwrap();
            assert_eq!(parsed.output, Some(PathBuf::from("a.json")));
        }
        let list = ["openapi", "export", "--force", "-o", "a.json"];
        assert!(parse(&args(&list)).unwrap().force);
    }

    #[test]
    fn bad_arguments_are_usage_errors() {
        for list in [
            ["openapi"].as_slice(),
            &["openapi", "import"],
            &["openapi", "export", "--yaml"],
            &["openapi", "export", "--output"],
            &["openapi", "export", "--output="],
            &["openapi", "export", "--force"],
        ] {
            let err = parse(&args(list)).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{list:?}");
        }
    }

    #[test]
    fn relative_outputs_become_absolute() {
        let cwd = Path::new("/work/dir");
        let raw = args(&["openapi", "export", "-o", "spec.json"]);
        let out = absolute_output(&raw, cwd);
        assert_eq!(out[3], "/work/dir/spec.json");
        let raw = args(&["openapi", "export", "--output=/abs.json"]);
        assert_eq!(absolute_output(&raw, cwd)[2], "--output=/abs.json");
        let raw = args(&["openapi", "export", "--output=a b.json"]);
        let out = absolute_output(&raw, cwd);
        assert_eq!(out[2], "--output=/work/dir/a b.json");
        assert!(writes_file(&raw));
        assert!(!writes_file(&args(&["openapi", "export"])));
    }
}
