//! Entry point of the standalone `siderite` binary.

use crate::args::split_global;
use crate::connect::{connect_url, scratch_db};
use crate::error::CliError;
use crate::report::{UrlSource, channel};
use crate::status::{STATUS_TIMEOUT, Selection, status};
use siderite_migrations::MigrationError;
use siderite_migrations::cli::{self, ExitCode};
use std::path::PathBuf;

/// JSON-file migrations when there is no application package.
///
/// # Errors
/// Returns a migration or usage error to print on stderr.
pub(crate) async fn run_with(
    raw: &[String],
    env_database_url: Option<String>,
) -> Result<ExitCode, MigrationError> {
    let (global, rest) = split_global(raw).map_err(into_migration_error)?;
    if global.help {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }
    let command = rest.first().map(String::as_str).unwrap_or("");
    if command.is_empty() {
        print_help();
        return Ok(ExitCode::USAGE);
    }
    if command == "makemigrations" {
        print_makemigrations_hint();
        return Ok(ExitCode::USAGE);
    }
    if matches!(
        command,
        "migrate" | "rollback" | "showmigrations" | "inspectmigrations" | "squashmigrations"
    ) {
        let dir = global
            .migrations_dir
            .unwrap_or_else(|| PathBuf::from("migrations"));
        let url = global.database_url.or(env_database_url);
        let db = if command == "squashmigrations" {
            // Squashing only reads and writes JSON files.
            match url.as_deref() {
                Some(url) => connect_url(url).await?,
                None => scratch_db().await?,
            }
        } else {
            let url =
                url.ok_or_else(|| MigrationError::usage("missing --database-url or DATABASE_URL"))?;
            connect_url(&url).await?
        };
        return cli::run(&[], &db, &dir, rest).await;
    }
    eprintln!("unknown command `{command}` (try --help)");
    Ok(ExitCode::USAGE)
}

/// `showmigrations --json` without an application package (`dispatch`
/// lets `--json` through only for the JSON commands).
///
/// Errors:
///   Usage errors, or the status errors of [`status`].
pub(crate) async fn run_json(
    raw: &[String],
    env_database_url: Option<String>,
) -> Result<u8, CliError> {
    let (global, rest) = split_global(raw)?;
    if rest.first().map(String::as_str) != Some("showmigrations") {
        return Err(CliError::usage(
            "only showmigrations supports --json without an application",
        ));
    }
    if let Some(extra) = rest.get(1) {
        let msg = format!("unexpected argument `{extra}`");
        return Err(CliError::usage(msg));
    }
    let (url, source) = match (global.database_url, env_database_url) {
        (Some(url), _) => (url, UrlSource::Flag),
        (None, Some(url)) if !url.is_empty() => (url, UrlSource::Environment),
        _ => {
            let msg = "missing --database-url or DATABASE_URL";
            return Err(CliError::usage(msg));
        }
    };
    let selection = Selection {
        url,
        alias: None,
        source,
    };
    let dir = global
        .migrations_dir
        .unwrap_or_else(|| PathBuf::from("migrations"));
    let report = status(&selection, &dir, STATUS_TIMEOUT).await?;
    channel::emit_json(&report, None)?;
    Ok(report.exit_code())
}

fn into_migration_error(err: CliError) -> MigrationError {
    MigrationError::usage(err.to_string())
}

fn print_help() {
    println!(
        "\
siderite {} — schema migrations (no application package)

Commands:
  migrate [TARGET]            Apply migrations
  rollback [--steps N|TARGET] Unapply migrations
  showmigrations [--json]     List migrations and applied status
  squashmigrations FROM TO    Collapse a range into one migration

Options:
  --database-url URL          sqlite:, postgres:// or mysql:// URL (or DATABASE_URL)
  --migrations-dir DIR        Migration JSON directory (default: migrations)
  --dry-run                   Print SQL without executing
  --name SLUG                 Slug for a new / squashed migration
  --empty                     Write an empty migration (app binary only)
  --steps N                   Rollback N applied migrations
  --help                      Show this help

PostgreSQL and MySQL URLs need a binary built with `--features postgres` /
`--features mysql`.
",
        env!("CARGO_PKG_VERSION")
    );
    print_makemigrations_hint();
}

fn print_makemigrations_hint() {
    println!(
        "\
makemigrations, run, routes, check and dbshell need your application.
`cd` into a `siderite new` project (or an example) or wire `AppCli`:

    #[tokio::main]
    async fn main() -> std::process::ExitCode {{
        siderite_cli::AppCli::new(app)
            .models(&[User::META, Post::META])
            .run()
            .await
    }}
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

    #[tokio::test]
    async fn help_and_missing_command() {
        assert_eq!(
            run_with(&args(&["--help"]), None).await.unwrap(),
            ExitCode::SUCCESS
        );
        assert_eq!(run_with(&[], None).await.unwrap(), ExitCode::USAGE);
    }

    #[tokio::test]
    async fn app_only_and_unknown_commands_are_usage_errors() {
        for command in ["makemigrations", "nope"] {
            assert_eq!(
                run_with(&args(&[command]), None).await.unwrap(),
                ExitCode::USAGE,
                "{command}"
            );
        }
    }

    #[tokio::test]
    async fn migrate_needs_a_database() {
        let err = run_with(&args(&["migrate"]), None).await.unwrap_err();
        assert!(err.to_string().contains("--database-url"));
    }

    #[tokio::test]
    async fn database_url_flag_wins_over_the_environment() {
        let dir = std::env::temp_dir().join(format!("siderite-standalone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let code = run_with(
            &args(&[
                "showmigrations",
                "--database-url",
                "sqlite::memory:",
                "--migrations-dir",
                dir.to_str().unwrap(),
            ]),
            Some("ftp://ignored".into()),
        )
        .await
        .unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[tokio::test]
    async fn unsupported_schemes_are_reported_without_the_url() {
        let err = run_with(
            &args(&["migrate", "--database-url", "ftp://u:hunter2@h/db"]),
            None,
        )
        .await
        .unwrap_err();
        assert!(!err.to_string().contains("hunter2"));
    }
}
