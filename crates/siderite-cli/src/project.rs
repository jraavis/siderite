//! Invoke `cargo run` / `cargo test`.

use crate::error::CliError;
use crate::report::channel::{self, NO_REPORT_CODE, REPORT_PATH_ENV};
use crate::workspace::spawn_error;
use std::path::Path;
use std::process::{Command, Stdio};

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
    let mut command = cargo_command(dir, "run", bin, args);
    exit_code(&mut command)
}

/// `cargo run` for a `--json` run of `command`: the application writes its
/// report to a private file named by [`REPORT_PATH_ENV`] and its stdout
/// (anything the app factory prints) goes to stderr. The report is then
/// printed as the only stdout output.
///
/// When the application exits without a valid report (a build failure, a
/// crash, or a `siderite-cli` that predates `--json`), a failure envelope
/// with code [`NO_REPORT_CODE`] is printed instead and the exit code is
/// `2` if the child exited `2`, else `1`.
pub fn cargo_run_report(
    dir: &Path,
    bin: Option<&str>,
    args: &[String],
    command_name: &str,
) -> CliResult {
    let report = ReportFile::new()?;
    let mut command = cargo_command(dir, "run", bin, args);
    command
        .env(REPORT_PATH_ENV, report.path())
        .stdout(Stdio::from(std::io::stderr()));
    let code = exit_code(&mut command)?;
    if let Some(rendered) = report.read() {
        channel::emit(&rendered, None)?;
        return Ok(if code <= 2 { code } else { 1 });
    }
    let message = format!(
        "the application exited with status {code} without writing a \
         report (see stderr; its siderite-cli may predate --json)"
    );
    let failure = channel::failure(command_name, NO_REPORT_CODE, &message);
    channel::emit_json(&failure, None)?;
    Ok(if code == 2 { 2 } else { 1 })
}

fn io_error(what: &str, path: &Path, err: &std::io::Error) -> CliError {
    CliError::Io(format!("cannot {what} {}: {err}", path.display()))
}

/// A unique report file under the temp dir, removed on drop.
struct ReportFile(std::path::PathBuf);

impl ReportFile {
    fn new() -> Result<Self, CliError> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let name = format!("siderite-report-{pid}-{nanos}-{n}.json");
        let path = std::env::temp_dir().join(name);
        // `create_new` refuses a file (or link) planted at the path.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|err| io_error("create", &path, &err))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// The report, when the child wrote exactly one JSON value.
    fn read(&self) -> Option<String> {
        let text = std::fs::read_to_string(&self.0).ok()?;
        serde_json::from_str::<serde_json::Value>(&text).ok()?;
        Some(text)
    }
}

impl Drop for ReportFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
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
    let mut command = cargo_command(package_dir, cargo_cmd, bin, &rest);
    exit_code(&mut command)
}

fn args_after(cargo_cmd: &str, args: &[String]) -> Vec<String> {
    args.iter()
        .skip_while(|a| a.as_str() != cargo_cmd)
        .skip(1)
        .cloned()
        .collect()
}

/// Every argument is a separate argv entry, so paths with spaces work.
fn cargo_command(
    package_dir: &Path,
    cargo_cmd: &str,
    bin: Option<&str>,
    args: &[String],
) -> Command {
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
    command
}

/// Run `command` and return its exit code (`1` when killed by a signal).
fn exit_code(command: &mut Command) -> CliResult {
    let status = command.status().map_err(|err| {
        let program = command.get_program().to_string_lossy();
        let sub = command
            .get_args()
            .next()
            .map(|a| a.to_string_lossy().into_owned())
            .unwrap_or_default();
        spawn_error(&format!("{program} {sub}"), &err)
    })?;
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
