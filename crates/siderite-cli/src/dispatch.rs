//! Top-level `siderite` binary: `new`, cargo wrap, or standalone migrations.

use crate::args::{check_json_args, json_requested};
use crate::doctor;
use crate::error::CliError;
use crate::openapi;
use crate::project::{self, CARGO_COMMANDS, Missing, is_app_command};
use crate::report::channel;
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
    if !json_requested(&raw) || CARGO_COMMANDS.contains(&command) {
        return Ok(());
    }
    channel::emit_json(&channel::error_envelope(command, err), None)
}

async fn dispatch(raw: &[String], cwd: &std::path::Path) -> Result<u8, CliError> {
    let (project, raw) = split_project(raw)?;
    let raw = raw.as_slice();
    let command = first_command(raw);
    let cargo = command.is_some_and(|c| CARGO_COMMANDS.contains(&c));
    if json_requested(raw) && !cargo {
        check_json_args(raw, command)?;
    }
    if raw.iter().any(|a| a == "--help" || a == "-h")
        && first_command(raw).is_none_or(|c| c == "help")
    {
        print_help();
        return Ok(0);
    }
    let command = first_command(raw).unwrap_or("");
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
    if command == "doctor" {
        return doctor::run(raw, cwd.to_path_buf(), project).await;
    }
    if CARGO_COMMANDS.contains(&command) {
        let found = target::locate("cargo", cwd, &project, false)?;
        let Selected { dir, bin, .. } = found.ok_or_else(|| {
            CliError::usage(format!(
                "{command} needs a Cargo package (cd into a siderite app, or run siderite new)"
            ))
        })?;
        return project::cargo_passthrough(&dir, command, bin.as_deref(), raw);
    }
    if is_app_command(command) {
        if let Some(app) = target::locate("cargo", cwd, &project, true)? {
            let bin = app.bin.as_deref();
            return run_app(&app.dir, bin, raw, command, cwd);
        }
        if NEEDS_APP.contains(&command) {
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

/// App commands that make no sense without an application package.
const NEEDS_APP: &[&str] = &[
    "run",
    "routes",
    "check",
    "dbshell",
    "makemigrations",
    "openapi",
];

/// `cargo run` the application with `raw`. Output that must stay clean on
/// stdout (`--json` reports, the OpenAPI document) is captured.
fn run_app(
    dir: &Path,
    bin: Option<&str>,
    raw: &[String],
    command: &str,
    cwd: &Path,
) -> Result<u8, CliError> {
    if json_requested(raw) {
        let missing = Missing::Envelope(command);
        return project::cargo_run_captured(dir, bin, raw, missing);
    }
    if command != "openapi" {
        return project::cargo_run(dir, bin, raw);
    }
    let raw = openapi::absolute_output(raw, cwd);
    if openapi::writes_file(&raw) {
        return project::cargo_run(dir, bin, &raw);
    }
    project::cargo_run_captured(dir, bin, &raw, Missing::Nothing)
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
    println!(
        "\
siderite {} — FastAPI-style Rust web framework

Create and run an app:
  new NAME                      Write a new API crate
  doctor [--json] [--live]      Check the toolchain, project and config
                                (--live: read-only database probes)
  run [--addr ADDR]             Serve the app
  routes [--json]               List METHOD PATH operation_id
  check [--json]                Validate config, models, migrations and routes
  openapi export [-o PATH] [--force]
                                Write the OpenAPI document (stdout or PATH)
  dbshell                       Open the database's native client
  build [--release ...]         cargo build in the app package
  test                          cargo test in the app package

Migrations:
  makemigrations [--name SLUG] [--empty] [--dry-run]
  migrate [TARGET] [--dry-run]
  rollback [--steps N | TARGET] [--dry-run]
  showmigrations
  inspectmigrations       Read-only recovery report
  squashmigrations FROM TO [--name SLUG]

Options:
  --addr ADDR                   Listen address (run)
  --database ALIAS              Database alias (migrate, dbshell)
  --database-url URL            Database URL
  --migrations-dir DIR          Migration JSON directory
  --manifest-path PATH          Cargo.toml of the app (before the command)
  --package, -p NAME            Workspace package of the app
  --bin NAME                    Binary of the app package
  --json                        One JSON report on stdout (check, routes)
  --help                        Show this help

`run`, `routes`, `check`, `dbshell` and `makemigrations` invoke `cargo run`
in the current package. `migrate` without a package uses JSON files and
`--database-url` / DATABASE_URL.
",
        env!("CARGO_PKG_VERSION")
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
