//! Top-level `siderite` binary: `new`, cargo wrap, or standalone migrations.

use crate::args::{self, GlobalArgs};
use crate::commands::{command_catalog, global_flags, render_commands_text};
use crate::completions::{self, Shell};
use crate::envelope::CliEnvelope;
use crate::error::CliError;
use crate::project::{self, CARGO_COMMANDS, is_app_command};
use crate::scaffold;
use crate::standalone;
use std::env;

/// Entry point of the `siderite` binary.
///
/// # Errors
/// Usage, IO, connection, or migration errors.
pub async fn run() -> Result<u8, CliError> {
    let raw: Vec<String> = env::args().skip(1).collect();
    let cwd = env::current_dir().map_err(|err| CliError::Io(format!("cannot read cwd: {err}")))?;
    match dispatch(&raw, &cwd).await {
        Ok(code) => Ok(code),
        Err(err) => {
            if raw.iter().any(|a| a == "--json") {
                let cmd = first_command(&raw).unwrap_or("siderite");
                let env: CliEnvelope<()> = CliEnvelope::error(cmd, "ERROR", err.to_string());
                if let Ok(json) = env.to_json_pretty() {
                    eprintln!("{json}");
                }
            } else {
                eprintln!("error: {err}");
            }
            Err(err)
        }
    }
}

async fn dispatch(raw: &[String], cwd: &std::path::Path) -> Result<u8, CliError> {
    let (global, _rest) = args::split_global(raw)?;
    if (raw.iter().any(|a| a == "--help" || a == "-h")
        && first_command(raw).is_none_or(|c| c == "help"))
        || first_command(raw) == Some("help")
    {
        return handle_help(&global);
    }
    let command = first_command(raw).unwrap_or("");
    if command.is_empty() {
        return handle_missing_command(&global);
    }
    if command == "commands" {
        return handle_commands(&global);
    }
    if command == "setup" {
        return crate::setup::run(cwd, &global);
    }
    if command == "doctor" {
        return crate::doctor::run(cwd, &global);
    }
    if command == "completions" {
        return handle_completions(raw);
    }
    if command == "docs" {
        return crate::docs::run(cwd, &global, raw);
    }
    if command == "explain" {
        return crate::explain::run(cwd, &global, raw);
    }
    if command == "verify" {
        return crate::verify::run(cwd, &global, raw);
    }
    if command == "new" {
        return scaffold::run(raw);
    }
    if CARGO_COMMANDS.contains(&command) {
        let resolved = project::resolve_project(cwd, &global, command)?;
        return project::cargo_passthrough(&resolved, command, raw);
    }
    if is_app_command(command) {
        match project::resolve_project(cwd, &global, command) {
            Ok(resolved) => return project::cargo_run(&resolved, raw),
            Err(err) => {
                if matches!(command, "run" | "routes" | "check" | "makemigrations") {
                    return Err(err);
                }
            }
        }
    }
    let code = standalone::run_with(raw, env::var("DATABASE_URL").ok()).await?;
    Ok(code.0)
}

fn handle_help(global: &GlobalArgs) -> Result<u8, CliError> {
    if global.json {
        let env = CliEnvelope::success("commands", command_catalog());
        let json = env
            .to_json_pretty()
            .map_err(|err| CliError::Io(format!("failed to serialize JSON: {err}")))?;
        println!("{json}");
    } else {
        print_help();
    }
    Ok(0)
}

fn handle_missing_command(global: &GlobalArgs) -> Result<u8, CliError> {
    if global.json {
        let env: CliEnvelope<()> =
            CliEnvelope::error("siderite", "USAGE", "no command specified; try --help");
        let json = env
            .to_json_pretty()
            .map_err(|err| CliError::Io(format!("failed to serialize JSON: {err}")))?;
        eprintln!("{json}");
    } else {
        print_help();
    }
    Ok(2)
}

fn handle_commands(global: &GlobalArgs) -> Result<u8, CliError> {
    if global.json {
        let env = CliEnvelope::success("commands", command_catalog());
        let json = env
            .to_json_pretty()
            .map_err(|err| CliError::Io(format!("failed to serialize JSON: {err}")))?;
        println!("{json}");
    } else {
        print!("{}", render_commands_text(&command_catalog()));
    }
    Ok(0)
}

fn handle_completions(raw: &[String]) -> Result<u8, CliError> {
    // Global flags and their values may precede the command; drop them first.
    let (_, rest) = args::split_global(raw)?;
    let mut positionals = rest
        .iter()
        .filter(|a| !a.starts_with('-'))
        .skip_while(|a| *a != "completions")
        .skip(1);
    let shell = positionals
        .next()
        .ok_or_else(|| CliError::Usage("usage: siderite completions <bash|zsh|fish>".into()))?
        .parse::<Shell>()
        .map_err(CliError::Usage)?;
    if let Some(extra) = positionals.next() {
        return Err(CliError::Usage(format!(
            "unexpected argument `{extra}`; usage: siderite completions <bash|zsh|fish>"
        )));
    }
    print!(
        "{}",
        completions::render(shell, &command_catalog(), &global_flags())
    );
    Ok(0)
}

/// First positional token, skipping flags and the values of known flags.
fn first_command(args: &[String]) -> Option<&str> {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "--help" || arg == "-h" || arg == "--json" {
            continue;
        }
        if let Some((flag, inline)) = flag_parts(arg) {
            if inline.is_none()
                && matches!(
                    flag,
                    "--database-url"
                        | "--database"
                        | "--addr"
                        | "--migrations-dir"
                        | "--manifest-path"
                        | "--package"
                        | "-p"
                        | "--bin"
                        | "--path"
                        | "--limit"
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
    if arg.starts_with("--") {
        return Some(match arg.split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (arg, None),
        });
    }
    if arg == "-p" {
        return Some(("-p", None));
    }
    if let Some(rest) = arg.strip_prefix("-p=") {
        return Some(("-p", Some(rest)));
    }
    None
}

fn print_help() {
    println!(
        "\
siderite {} — FastAPI-style Rust web framework

Create and run an app:
  new NAME                      Write a new API crate
  run [--addr ADDR]             Serve the app
  routes [--json]               List METHOD PATH operation_id
  check [--json]                Validate config, models, migrations and routes
  dbshell                       Open the database's native client
  build [--release ...]         cargo build in the app package
  test                          cargo test in the app package
  fmt [--check]                 cargo fmt in the app package
  lint [-- -D warnings]         cargo clippy (Rust lints, not `check`)
  clean                         cargo clean in the app package
  verify [--json]               fmt --check, lint, build, test and check
  commands [--json]             List available commands and metadata
  setup [--json]                Check Rust prerequisites; print next steps
  doctor [--json]               Offline toolchain, project and config checks
  completions SHELL             Print a bash, zsh or fish completion script
  docs search QUERY [--limit N] Search the offline framework guides
  explain CODE | --list         Explain a `check` issue id and its fix

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
  --manifest-path PATH          Path to Cargo.toml
  -p, --package PKG             Target package in workspace
  --bin BIN                     Target binary
  --json                        Produce structured JSON output
  --help                        Show this help

`run`, `routes`, `check`, `dbshell` and `makemigrations` invoke `cargo run`
in the current package; `build`, `test`, `fmt`, `lint` and `clean` run the
matching cargo command there. `migrate` without a package uses JSON files and
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
        assert_eq!(first_command(&args(&["-p", "demo", "run"])), Some("run"));
        assert_eq!(
            first_command(&args(&["--manifest-path", "Cargo.toml", "build"])),
            Some("build")
        );
        assert_eq!(
            first_command(&args(&["--json", "commands"])),
            Some("commands")
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
    async fn commands_command_returns_success() {
        let cwd = std::env::temp_dir();
        assert_eq!(dispatch(&args(&["commands"]), &cwd).await.unwrap(), 0);
        assert_eq!(
            dispatch(&args(&["commands", "--json"]), &cwd)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn completions_work_outside_a_project() {
        let dir = std::env::temp_dir().join(format!("siderite-compl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for shell in ["bash", "zsh", "fish"] {
            assert_eq!(
                dispatch(&args(&["completions", shell]), &dir)
                    .await
                    .unwrap(),
                0
            );
        }
        for prefixed in [
            &["-p", "demo", "completions", "bash"][..],
            &["--manifest-path", "Cargo.toml", "completions", "zsh"],
            &["completions", "--bin=app", "fish"],
        ] {
            assert_eq!(
                dispatch(&args(prefixed), &dir).await.unwrap(),
                0,
                "{prefixed:?}"
            );
        }
        for bad in [
            &["completions"][..],
            &["completions", "tcsh"],
            &["completions", "zsh", "x"],
        ] {
            let err = dispatch(&args(bad), &dir).await.unwrap_err();
            assert_eq!(err.exit_code(), 2, "{bad:?}");
        }
    }

    #[tokio::test]
    async fn setup_and_doctor_run_outside_a_project() {
        let dir = std::env::temp_dir().join(format!("siderite-setup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for cmd in [
            &["setup"][..],
            &["setup", "--json"],
            &["doctor"],
            &["doctor", "--json"],
        ] {
            let code = dispatch(&args(cmd), &dir).await.unwrap();
            assert!(code <= 1, "{cmd:?} -> {code}");
        }
    }

    #[tokio::test]
    async fn run_without_a_package_is_usage() {
        let dir = std::env::temp_dir().join(format!("siderite-dispatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = dispatch(&args(&["run"]), &dir).await.unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("needs an application"));
    }

    #[tokio::test]
    async fn cargo_commands_without_a_package_are_usage() {
        let dir = std::env::temp_dir().join(format!("siderite-fmt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for cmd in ["fmt", "lint", "clean", "verify"] {
            let err = dispatch(&args(&[cmd]), &dir).await.unwrap_err();
            assert_eq!(err.exit_code(), 2, "{cmd}");
            assert!(err.to_string().contains(&format!("`{cmd}` needs")));
        }
    }

    #[tokio::test]
    async fn build_without_a_package_is_usage() {
        let dir = std::env::temp_dir().join(format!("siderite-build-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = dispatch(&args(&["build", "--release"]), &dir)
            .await
            .unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("needs an application"));
    }
}
