//! Global flags shared by standalone binary and [`AppCli`](crate::AppCli).
//!
//! Global flags may appear anywhere on the command line. Everything else is
//! passed through untouched, in order, for the command to parse (the migration
//! commands parse their own flags in `siderite_migrations::cli`).

use crate::error::CliError;
use std::path::PathBuf;

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
    /// `--manifest-path PATH`.
    pub manifest_path: Option<PathBuf>,
    /// `--package PKG` / `-p PKG`.
    pub package: Option<String>,
    /// `--bin BIN`.
    pub bin: Option<String>,
    /// `--json`.
    pub json: bool,
    /// `--help` / `-h` before any command.
    pub help: bool,
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
        if arg == "--json" {
            global.json = true;
            rest.push(arg.clone());
            continue;
        }
        let Some((flag, inline)) = flag_parts(arg) else {
            rest.push(arg.clone());
            continue;
        };
        let slot = match flag {
            "--database-url" => &mut global.database_url,
            "--database" => &mut global.database,
            "--addr" => &mut global.addr,
            "--manifest-path" => {
                let value = flag_value(flag, inline, args, &mut i)?;
                global.manifest_path = Some(PathBuf::from(value));
                continue;
            }
            "--package" | "-p" => &mut global.package,
            "--bin" => &mut global.bin,
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

/// `--flag=val` -> (`--flag`, Some(`val`)); `--flag` -> (`--flag`, None).
/// Also handles `-p=val` -> (`-p`, Some(`val`)) and `-p` -> (`-p`, None).
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
            "-p",
            "myapp",
            "--manifest-path=crates/app/Cargo.toml",
            "--bin=server",
            "--json",
            "--dry-run",
        ])
        .unwrap();
        assert_eq!(global.database_url.as_deref(), Some("sqlite::memory:"));
        assert_eq!(global.database.as_deref(), Some("replica"));
        assert_eq!(global.addr.as_deref(), Some("0.0.0.0:1"));
        assert_eq!(global.migrations_dir, Some(PathBuf::from("db/migrations")));
        assert_eq!(global.package.as_deref(), Some("myapp"));
        assert_eq!(
            global.manifest_path,
            Some(PathBuf::from("crates/app/Cargo.toml"))
        );
        assert_eq!(global.bin.as_deref(), Some("server"));
        assert!(global.json);
        assert_eq!(rest, ["migrate", "0001_init", "--json", "--dry-run"]);
        assert!(!global.help);
    }

    #[test]
    fn parses_short_p_flag_with_equal() {
        let (global, rest) = split(&["run", "-p=demo"]).unwrap();
        assert_eq!(global.package.as_deref(), Some("demo"));
        assert_eq!(rest, ["run"]);
    }

    #[test]
    fn missing_values_are_usage_errors() {
        for flag in [
            "--database-url",
            "--database",
            "--addr",
            "--migrations-dir",
            "--manifest-path",
            "--package",
            "-p",
            "--bin",
        ] {
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
    fn unknown_flags_and_values_pass_through() {
        let (_, rest) = split(&["rollback", "--steps", "2", "-x"]).unwrap();
        assert_eq!(rest, ["rollback", "--steps", "2", "-x"]);
    }
}
