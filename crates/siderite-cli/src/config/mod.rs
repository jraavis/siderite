//! `siderite config show [--redacted] [--json]` and `siderite config
//! validate [--json]`: the effective configuration of the project here.
//!
//! Both read the same sources as `siderite_config::load` (and `doctor`):
//! `siderite.toml` in the package directory, `SIDERITE_*` variables and
//! the `DATABASE_URL` / `ADDR` aliases. Values are always redacted:
//! secret keys show `***` (or a URL without its credentials), and other
//! text loses any credential it holds. `--redacted` states that
//! explicitly; there is no flag that prints secrets.

use crate::check::config::check_url;
use crate::error::CliError;
use crate::report::build::diagnostic;
use crate::report::text::diagnostic_line;
use crate::report::{Diagnostic, Envelope, Severity, channel};
use crate::target::{self, ProjectArgs};
use serde::Serialize;
use siderite_config::DEFAULT_CONFIG_FILE as FILE;
use siderite_config::{ConfigBuilder, ConfigEntry, DEFAULT_ENV_PREFIX};
use siderite_config::{Settings, SourceKind};
use std::fmt::Write;
use std::path::Path;

#[cfg(test)]
mod tests;

const USAGE: &str = "usage: config show [--redacted] [--json] | \
                     config validate [--json]";

/// The subcommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Print every value and its source.
    Show,
    /// Load and check the configuration.
    Validate,
}

/// Parsed arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Options {
    /// The subcommand.
    pub action: Action,
    /// `--json`.
    pub json: bool,
}

/// Parse the arguments after the project flags (`config ...`).
///
/// Errors:
///   A usage error for a missing subcommand or an unknown flag.
pub(crate) fn parse(raw: &[String]) -> Result<Options, CliError> {
    let mut args = raw.iter().skip_while(|a| *a != "config").skip(1);
    let action = match args.next().map(String::as_str) {
        Some("show") => Action::Show,
        Some("validate") => Action::Validate,
        _ => return Err(CliError::usage(USAGE)),
    };
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--redacted" if action == Action::Show => {}
            other => {
                let msg = format!("unexpected argument `{other}`; {USAGE}");
                return Err(CliError::usage(msg));
            }
        }
    }
    Ok(Options { action, json })
}

/// The builder `siderite_config::load` uses, rooted at `dir`.
///
/// Args:
///   dir: The package directory holding `siderite.toml`.
///   env: Whether to read the process environment.
///
/// Returns:
///   The builder; nothing is read until it is built.
pub(crate) fn builder(dir: &Path, env: bool) -> ConfigBuilder {
    let builder = ConfigBuilder::new().file_optional(dir.join(FILE));
    if env {
        builder.env_prefix(DEFAULT_ENV_PREFIX)
    } else {
        builder
    }
}

/// The `data` of a `config` envelope.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ConfigData {
    /// The configuration file, whether or not it exists.
    pub file: String,
    /// Whether the file exists.
    pub file_exists: bool,
    /// Every resolved value, by key (empty for `validate`).
    pub entries: Vec<ConfigEntry>,
}

type Report = Envelope<ConfigData>;
type Exit = Result<u8, CliError>;

/// Load the configuration of `dir` and report it.
///
/// Args:
///   options: The parsed arguments.
///   dir: The package directory.
///   env: Whether to read the process environment.
///
/// Returns:
///   The report: entries for `show`, and a diagnostic for each problem.
pub(crate) fn report(options: Options, dir: &Path, env: bool) -> Report {
    let builder = builder(dir, env);
    let file = dir.join(FILE);
    let mut diags = Vec::new();
    match builder.build() {
        Ok(settings) => diags.extend(urls(&settings)),
        Err(err) => diags.push(load_error(&err.to_string(), &file)),
    }
    let entries = match options.action {
        Action::Show => builder.explain().unwrap_or_default(),
        Action::Validate => Vec::new(),
    };
    let data = ConfigData {
        file: file.display().to_string(),
        file_exists: file.exists(),
        entries,
    };
    let command = match options.action {
        Action::Show => "config show",
        Action::Validate => "config validate",
    };
    Envelope::new(command, data, diags)
}

/// `config.E004`: the sources do not load as settings. The message is
/// already redacted by `siderite_config`.
fn load_error(message: &str, file: &Path) -> Diagnostic {
    let location = file.display().to_string();
    let prefix = DEFAULT_ENV_PREFIX;
    let hint = format!("fix {location} or the {prefix}* variables");
    let diag = Diagnostic::new("config.E004", Severity::Error, message);
    diag.with_location(&location).with_hint(&hint)
}

/// The URL checks `siderite check` runs, on the loaded settings.
fn urls(settings: &Settings) -> Vec<Diagnostic> {
    let found = settings.databases.iter().filter_map(|(alias, db)| {
        let issue = check_url(alias, db.url.expose())?;
        Some(diagnostic(&issue))
    });
    found.collect()
}

/// Text form: one `key = value  (source)` line per entry for `show`, a
/// one-line verdict for `validate`; diagnostics follow.
pub(crate) fn render(report: &Report, action: Action) -> String {
    let data = &report.data;
    let mut out = String::new();
    let file = if data.file_exists {
        data.file.clone()
    } else {
        format!("{} (absent)", data.file)
    };
    let _ = writeln!(out, "config file: {file}");
    if action == Action::Show {
        for entry in &data.entries {
            let _ = writeln!(out, "{}", line(entry));
        }
    }
    for diag in &report.diagnostics {
        let _ = writeln!(out, "{}", diagnostic_line(diag));
    }
    if action == Action::Validate && report.ok {
        let _ = writeln!(out, "configuration is valid");
    }
    out
}

fn line(entry: &ConfigEntry) -> String {
    let source = &entry.source;
    let kind = match source.kind {
        SourceKind::Default => "default",
        SourceKind::File => "file",
        SourceKind::Environment => "env",
        SourceKind::Override => "override",
    };
    let origin = match &source.detail {
        Some(detail) => format!("{kind} {detail}"),
        None => kind.to_owned(),
    };
    format!("{} = {}  ({origin})", entry.key, entry.value)
}

/// `siderite config` from `cwd`.
///
/// Errors:
///   Usage errors, a package that cannot be selected, or an IO error
///   writing the report.
pub(crate) fn run(raw: &[String], cwd: &Path, args: &ProjectArgs) -> Exit {
    let options = parse(raw)?;
    let dir = target::package_dir(cwd, args)?;
    let report = report(options, &dir, true);
    if options.json {
        channel::emit_json(&report, None)?;
    } else {
        print!("{}", render(&report, options.action));
    }
    Ok(report.exit_code())
}
