//! Invoke `cargo run` / `cargo test`.

use crate::error::CliError;
use crate::workspace::spawn_error;
use std::path::Path;
use std::process::Command;

/// Commands that `AppCli` in the application binary understands.
pub const APP_COMMANDS: &[&str] = &[
    "run",
    "routes",
    "check",
    "dbshell",
    "makemigrations",
    "migrate",
    "rollback",
    "showmigrations",
    "inspectmigrations",
    "squashmigrations",
];

/// Whether `command` is forwarded to the application binary.
pub fn is_app_command(command: &str) -> bool {
    APP_COMMANDS.contains(&command)
}

type CliResult = Result<u8, CliError>;

/// `cargo run [--bin BIN] -- args` in `package_dir`.
///
/// Forwards the child's exit code.
pub fn cargo_run(dir: &Path, bin: Option<&str>, args: &[String]) -> CliResult {
    cargo(dir, "run", bin, args)
}

/// Cargo subcommands `siderite` forwards verbatim in the app package.
pub const CARGO_COMMANDS: &[&str] = &["build", "test"];

/// `cargo <cargo_cmd>` in `package_dir` with the args after `cargo_cmd`
/// (no extra `--` unless the user passed some).
pub fn cargo_passthrough(
    package_dir: &Path,
    cargo_cmd: &str,
    bin: Option<&str>,
    args: &[String],
) -> Result<u8, CliError> {
    let rest = args_after(cargo_cmd, args);
    cargo(package_dir, cargo_cmd, bin, &rest)
}

fn args_after(cargo_cmd: &str, args: &[String]) -> Vec<String> {
    args.iter()
        .skip_while(|a| a.as_str() != cargo_cmd)
        .skip(1)
        .cloned()
        .collect()
}

/// Every argument is a separate argv entry, so paths with spaces work.
fn cargo(
    package_dir: &Path,
    cargo_cmd: &str,
    bin: Option<&str>,
    args: &[String],
) -> Result<u8, CliError> {
    let mut command = Command::new("cargo");
    command.arg(cargo_cmd).current_dir(package_dir);
    if let Some(bin) = bin {
        command.arg("--bin").arg(bin);
    }
    if cargo_cmd == "run" {
        command.arg("--").args(args);
    } else {
        command.args(args);
    }
    let status = command
        .status()
        .map_err(|err| spawn_error(&format!("cargo {cargo_cmd}"), &err))?;
    Ok(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_forwards_args_after_the_command() {
        let args: Vec<String> = ["build", "--release", "--features", "x"]
            .iter()
            .map(|a| (*a).to_owned())
            .collect();
        assert_eq!(args_after("build", &args), args[1..].to_vec());
        assert!(args_after("test", &args[..1]).is_empty());
    }
}
