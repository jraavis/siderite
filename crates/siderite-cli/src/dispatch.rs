//! Top-level `siderite` binary: `new`, cargo wrap, or standalone migrations.

use crate::args::{check_json_args, json_requested};
use crate::commands::{self, Runner};
use crate::doctor;
use crate::error::CliError;
use crate::openapi;
use crate::project::{self, Output, passthrough, passthrough_of};
use crate::report::{Envelope, channel};
use crate::scaffold;
use crate::standalone;
use crate::target::{self, ProjectArgs, Selected, split_project};
use std::env;
use std::path::Path;

/// Entry point of the `siderite` binary.
///
/// # Errors
/// Usage, IO, connection, or migration errors.
pub async fn run() -> Result<u8, CliError> {
    let raw: Vec<String> = env::args().skip(1).collect();
    let cwd = env::current_dir().map_err(|err| CliError::Io(format!("cannot read cwd: {err}")))?;
    let outcome = dispatch(&raw, &cwd).await;
    if let Err(err) = &outcome
        && let Err(write) = report_json_error(&raw, err)
    {
        // The original error decides the exit code, not this write.
        eprintln!("{write}");
    }
    outcome
}

/// In JSON mode, write a failure of the `siderite` binary itself as an
/// envelope on stdout (cargo passthroughs keep their own output).
fn report_json_error(raw: &[String], err: &CliError) -> Result<(), CliError> {
    let Ok((_, raw)) = split_project(raw) else {
        return Ok(());
    };
    let command = first_command(&raw).unwrap_or("siderite");
    if !json_requested(&raw) || passthrough(command).is_some() {
        return Ok(());
    }
    channel::emit_json(&channel::error_envelope(command, err), None)
}

async fn dispatch(raw: &[String], cwd: &std::path::Path) -> Result<u8, CliError> {
    let (project, raw) = split_project(raw)?;
    let raw = raw.as_slice();
    let command = first_command(raw);
    // The command contract is consulted once per run.
    let spec = command.and_then(commands::find);
    let cargo = spec.and_then(passthrough_of);
    if cargo.is_none() && json_requested(raw) {
        check_json_args(raw, command)?;
    }
    let help = raw.iter().any(|a| a == "--help" || a == "-h");
    if help && command.is_none_or(|c| c == "help") {
        print_help();
        return Ok(0);
    }
    let command = command.unwrap_or("");
    if command.is_empty() || command == "help" {
        print_help();
        return Ok(if command == "help" { 0 } else { 2 });
    }
    if command == "new" {
        if project != ProjectArgs::default() {
            return Err(CliError::usage(
                "--manifest-path, --package and --bin do not apply to `new`",
            ));
        }
        return scaffold::run(raw);
    }
    if command == "completions" {
        return crate::completions::run(raw);
    }
    if command == "commands" {
        return list_commands(raw);
    }
    if command == "dev" {
        return crate::dev::run(raw, cwd, &project).await;
    }
    if command == "explain" {
        return crate::explain::run(raw);
    }
    if command == "docs" {
        return crate::docs::run(raw, cwd);
    }
    if command == "setup" {
        return doctor::setup::run(raw, cwd);
    }
    if command == "doctor" {
        return doctor::run(raw, cwd.to_path_buf(), project).await;
    }
    if command == "verify" {
        let app = target::locate("cargo", cwd, &project, true)?;
        let missing = || CliError::usage(NEEDS_APP);
        let app = app.ok_or_else(missing)?;
        return crate::verify::run(raw, &app.dir, app.bin.as_deref());
    }
    if let Some(pass) = &cargo {
        pass.check_bin(project.bin.as_deref())?;
        let found = target::locate("cargo", cwd, &project, false)?;
        let Selected { dir, bin, .. } = found.ok_or_else(|| {
            CliError::usage(format!(
                "{command} needs a Cargo package (cd into a siderite app, or run siderite new)"
            ))
        })?;
        return project::cargo_passthrough(&dir, pass, bin.as_deref(), raw);
    }
    let runner = spec.map(|s| s.runner);
    if matches!(runner, Some(Runner::App | Runner::AppOrStandalone)) {
        if let Some(app) = target::locate("cargo", cwd, &project, true)? {
            let bin = app.bin.as_deref();
            return run_app(&app.dir, bin, raw, command, cwd);
        }
        if runner == Some(Runner::App) {
            return Err(CliError::usage(format!(
                "`{command}` needs an application. cd into a project created with `siderite new`, or an example directory"
            )));
        }
    }
    let env_url = env::var("DATABASE_URL").ok();
    if json_requested(raw) {
        return standalone::run_json(raw, env_url).await;
    }
    let code = standalone::run_with(raw, env_url).await?;
    Ok(code.0)
}

/// `verify` outside an application.
const NEEDS_APP: &str = "`verify` needs an application; cd into one";

/// `siderite commands [--json]`.
fn list_commands(raw: &[String]) -> Result<u8, CliError> {
    let extra = raw.iter().find(|a| *a != "commands" && *a != "--json");
    if let Some(extra) = extra {
        let msg = format!("unexpected argument `{extra}` for `commands`");
        return Err(CliError::usage(msg));
    }
    if json_requested(raw) {
        let report = Envelope::new("commands", commands::list(), Vec::new());
        channel::emit_json(&report, None)?;
    } else {
        print!("{}", commands::table());
    }
    Ok(0)
}

/// `cargo run` the application with `raw`. Output that must stay clean on
/// stdout (`--json` reports, the OpenAPI document) is captured.
fn run_app(
    dir: &Path,
    bin: Option<&str>,
    raw: &[String],
    command: &str,
    cwd: &Path,
) -> Result<u8, CliError> {
    let ai_context = command == "ai" && raw.iter().any(|a| a == "context");
    let absolute;
    let raw = if ai_context {
        absolute = crate::args::absolutize(raw, cwd, &["--include"]);
        absolute.as_slice()
    } else {
        raw
    };
    if json_requested(raw) {
        let missing = Output::Envelope(command);
        return project::cargo_run_captured(dir, bin, raw, missing);
    }
    let ai_output = ["dump-context", "context"];
    if command == "ai" && raw.iter().any(|a| ai_output.contains(&a.as_str())) {
        // The context is the output: keep factory prints off stdout.
        let output = if format_json(raw) {
            Output::Json
        } else {
            Output::Text
        };
        return project::cargo_run_captured(dir, bin, raw, output);
    }
    if command != "openapi" {
        return project::cargo_run(dir, bin, raw);
    }
    let raw = openapi::absolute_output(raw, cwd);
    if openapi::writes_file(&raw) {
        return project::cargo_run(dir, bin, &raw);
    }
    project::cargo_run_captured(dir, bin, &raw, Output::Json)
}

/// Whether `--format json` (or `=json`) was asked for.
fn format_json(raw: &[String]) -> bool {
    let joined = raw.windows(2).any(|w| w[0] == "--format" && w[1] == "json");
    joined || raw.iter().any(|a| a == "--format=json")
}

/// First positional token, skipping flags and the values of known flags.
fn first_command(args: &[String]) -> Option<&str> {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "--help" || arg == "-h" {
            continue;
        }
        if let Some((flag, inline)) = flag_parts(arg) {
            if inline.is_none()
                && matches!(
                    flag,
                    "--database-url" | "--database" | "--addr" | "--migrations-dir" | "--path"
                )
            {
                i += 1;
            }
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg);
    }
    None
}

fn flag_parts(arg: &str) -> Option<(&str, Option<&str>)> {
    if !arg.starts_with("--") {
        return None;
    }
    Some(match arg.split_once('=') {
        Some((flag, value)) => (flag, Some(value)),
        None => (arg, None),
    })
}

fn print_help() {
    let version = env!("CARGO_PKG_VERSION");
    let commands = commands::help(|_| true);
    println!(
        "\
siderite {version} — FastAPI-style Rust web framework
{commands}
Options (before the command for the project flags):
  --manifest-path PATH            Cargo.toml of the app
  --package, -p NAME              Workspace package of the app
  --bin NAME                      Binary of the app package
  --database ALIAS                Database alias (migrations, dbshell)
  --database-url URL              Database URL
  --migrations-dir DIR            Migration JSON directory
  --addr ADDR                     Listen address (run)
  --json                          One JSON report on stdout
  --help                          Show this help

App commands run in your binary through `cargo run`; `siderite commands
--json` lists every command with what it needs and may change.
"
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn first_command_skips_flags() {
        assert_eq!(
            first_command(&args(&["--addr", "127.0.0.1:1", "run"])),
            Some("run")
        );
        assert_eq!(first_command(&args(&["new", "demo"])), Some("new"));
        assert_eq!(first_command(&args(&["--help"])), None);
    }

    #[tokio::test]
    async fn help_and_missing_command() {
        let cwd = std::env::temp_dir();
        assert_eq!(dispatch(&args(&["--help"]), &cwd).await.unwrap(), 0);
        assert_eq!(dispatch(&[], &cwd).await.unwrap(), 2);
        assert_eq!(dispatch(&args(&["help"]), &cwd).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn run_without_a_package_is_usage() {
        let dir = std::env::temp_dir().join(format!("siderite-dispatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = dispatch(&args(&["run"]), &dir).await.unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("siderite new"));
    }

    #[tokio::test]
    async fn build_without_a_package_is_usage() {
        let dir = std::env::temp_dir().join(format!("siderite-build-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = dispatch(&args(&["build", "--release"]), &dir)
            .await
            .unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("build needs a Cargo package"));
    }
}
