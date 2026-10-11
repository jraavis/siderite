//! Global flags shared by the standalone `siderite` binary and [`AppCli`](crate::AppCli).
//!
//! Global flags may appear anywhere on the command line. Everything else is
//! passed through untouched, in order, for the command to parse (the migration
//! commands parse their own flags in `siderite_migrations::cli`).

use crate::error::CliError;
use std::path::PathBuf;

type Usage = Result<(), CliError>;

/// Flags recognised before a command runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalArgs {
    /// `--database-url URL`.
    pub database_url: Option<String>,
    /// `--database ALIAS`.
    pub database: Option<String>,
    /// `--migrations-dir DIR`.
    pub migrations_dir: Option<PathBuf>,
    /// `--addr ADDR` (`run`).
    pub addr: Option<String>,
    /// `--help` / `-h` before any command.
    pub help: bool,
    /// `--json`: write one versioned JSON report to stdout.
    pub json: bool,
}

/// Split `args` into [`GlobalArgs`] and the remaining arguments.
///
/// # Errors
/// A usage error when a flag that needs a value has none.
pub fn split_global(args: &[String]) -> Result<(GlobalArgs, Vec<String>), CliError> {
    let mut global = GlobalArgs::default();
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "--help" || arg == "-h" {
            global.help = rest.is_empty();
            rest.push(arg.clone());
            continue;
        }
        let Some((flag, inline)) = flag_parts(arg) else {
            rest.push(arg.clone());
            continue;
        };
        if flag == "--json" {
            if inline.is_some() {
                return Err(CliError::usage("--json takes no value"));
            }
            global.json = true;
            continue;
        }
        let slot = match flag {
            "--database-url" => &mut global.database_url,
            "--database" => &mut global.database,
            "--addr" => &mut global.addr,
            "--migrations-dir" => {
                let value = flag_value(flag, inline, args, &mut i)?;
                global.migrations_dir = Some(PathBuf::from(value));
                continue;
            }
            _ => {
                rest.push(arg.clone());
                continue;
            }
        };
        *slot = Some(flag_value(flag, inline, args, &mut i)?);
    }
    Ok((global, rest))
}

/// Whether `args` ask for JSON output, even when they fail to parse (so
/// the parse error itself can be reported as JSON).
pub fn json_requested(args: &[String]) -> bool {
    args.iter()
        .any(|a| a == "--json" || a.starts_with("--json="))
}

/// The commands that support `--json`.
pub const JSON_COMMANDS: &[&str] = &[
    "check",
    "doctor",
    "routes",
    // A live read, explicitly requested.
    "showmigrations",
];

/// Validate a `--json` command line: it needs one of [`JSON_COMMANDS`]
/// and cannot be combined with `--help`.
///
/// Errors:
///   A usage error naming the problem.
pub fn check_json_args(args: &[String], command: Option<&str>) -> Usage {
    let Some(command) = command else {
        return Err(CliError::usage(
            "--json needs a command: check, doctor, routes or showmigrations",
        ));
    };
    if !JSON_COMMANDS.contains(&command) {
        return Err(CliError::usage(format!(
            "--json is supported by `check`, `doctor`, `routes` and \
             `showmigrations`, not `{command}`"
        )));
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Err(CliError::usage("--help cannot be combined with --json"));
    }
    Ok(())
}

/// `--flag=value` -> (`--flag`, Some(`value`)); `--flag` -> (`--flag`, None).
fn flag_parts(arg: &str) -> Option<(&str, Option<&str>)> {
    if !arg.starts_with("--") {
        return None;
    }
    Some(match arg.split_once('=') {
        Some((flag, value)) => (flag, Some(value)),
        None => (arg, None),
    })
}

fn flag_value(
    flag: &str,
    inline: Option<&str>,
    args: &[String],
    i: &mut usize,
) -> Result<String, CliError> {
    if let Some(value) = inline {
        return Ok(value.to_owned());
    }
    let value = args
        .get(*i)
        .ok_or_else(|| CliError::usage(format!("{flag} requires a value")))?;
    *i += 1;
    Ok(value.clone())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn split(args: &[&str]) -> Result<(GlobalArgs, Vec<String>), CliError> {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        split_global(&args)
    }

    #[test]
    fn parses_flags_anywhere_in_both_forms() {
        let (global, rest) = split(&[
            "migrate",
            "--database-url",
            "sqlite::memory:",
            "0001_init",
            "--migrations-dir=db/migrations",
            "--database=replica",
            "--addr",
            "0.0.0.0:1",
            "--dry-run",
        ])
        .unwrap();
        assert_eq!(global.database_url.as_deref(), Some("sqlite::memory:"));
        assert_eq!(global.database.as_deref(), Some("replica"));
        assert_eq!(global.addr.as_deref(), Some("0.0.0.0:1"));
        assert_eq!(global.migrations_dir, Some(PathBuf::from("db/migrations")));
        assert_eq!(rest, ["migrate", "0001_init", "--dry-run"]);
        assert!(!global.help);
    }

    #[test]
    fn missing_values_are_usage_errors() {
        for flag in ["--database-url", "--database", "--addr", "--migrations-dir"] {
            let err = split(&["migrate", flag]).unwrap_err();
            assert!(err.to_string().contains(flag), "{err}");
            assert_eq!(err.exit_code(), 2);
        }
    }

    #[test]
    fn leading_help_is_global_and_later_help_is_passed_on() {
        let (global, rest) = split(&["--help"]).unwrap();
        assert!(global.help);
        assert_eq!(rest, ["--help"]);
        let (global, rest) = split(&["migrate", "--help"]).unwrap();
        assert!(!global.help);
        assert_eq!(rest, ["migrate", "--help"]);
    }

    #[test]
    fn json_is_a_flag_anywhere_without_a_value() {
        for list in [["--json", "check"], ["check", "--json"]] {
            let (global, rest) = split(&list).unwrap();
            assert!(global.json);
            assert_eq!(rest, ["check"]);
        }
        let err = split(&["routes", "--json=1"]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn unknown_flags_and_values_pass_through() {
        let (_, rest) = split(&["rollback", "--steps", "2", "-x"]).unwrap();
        assert_eq!(rest, ["rollback", "--steps", "2", "-x"]);
    }
}
