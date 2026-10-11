//! Invoke `cargo run` / `cargo test`.

use crate::error::CliError;
use crate::fsio;
use crate::report::channel::{self, NO_REPORT_CODE, REPORT_PATH_ENV};
use crate::workspace::spawn_error;
use std::path::Path;
use std::process::{Command, Stdio};

/// Commands that `AppCli` in the application binary understands.
pub const APP_COMMANDS: &[&str] = &[
    "run",
    "ai",
    "openapi",
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
type Done = Result<(), CliError>;

/// `cargo run [--bin BIN] -- args` in `package_dir`.
///
/// Forwards the child's exit code.
pub fn cargo_run(dir: &Path, bin: Option<&str>, args: &[String]) -> CliResult {
    let mut command = cargo_command(dir, "run", bin, args);
    exit_code(&mut command)
}

/// The output [`cargo_run_captured`] expects from the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output<'a> {
    /// A JSON report envelope of this command (`--json` runs): when it
    /// is missing, a failure envelope is printed instead.
    Envelope(&'a str),
    /// A raw JSON document (the OpenAPI export).
    Json,
    /// Text (the Markdown AI context).
    Text,
}

/// `cargo run` whose stdout output must stay clean: the application
/// writes it to a private file named by [`REPORT_PATH_ENV`], its stdout
/// (anything the app factory prints) goes to stderr, and the file is then
/// printed as the only stdout output.
///
/// When the application exits without writing its output (for JSON
/// outputs: exactly one JSON value) after a build failure, a crash, or
/// with a `siderite-cli` without the command, stdout gets a failure
/// envelope for [`Output::Envelope`] and nothing otherwise (the reason
/// goes to stderr); the exit code is `2` if the child exited `2`, else
/// `1`.
pub fn cargo_run_captured(
    dir: &Path,
    bin: Option<&str>,
    args: &[String],
    output: Output<'_>,
) -> CliResult {
    let report = ReportFile::new()?;
    let mut command = cargo_command(dir, "run", bin, args);
    command
        .env(REPORT_PATH_ENV, report.path())
        .stdout(Stdio::from(std::io::stderr()));
    let code = exit_code(&mut command)?;
    if let Some(rendered) = report.read(output != Output::Text) {
        channel::emit(&rendered, None)?;
        return Ok(if code <= 2 { code } else { 1 });
    }
    let message = format!(
        "the application exited with status {code} without writing its \
         output (see stderr; its siderite-cli may predate this command)"
    );
    match output {
        Output::Envelope(name) => {
            let failure = channel::failure(name, NO_REPORT_CODE, &message);
            channel::emit_json(&failure, None)?;
        }
        Output::Json | Output::Text => eprintln!("error: {message}"),
    }
    Ok(if code == 2 { 2 } else { 1 })
}

/// A fresh, private output file under the temp dir, removed on drop.
struct ReportFile(std::path::PathBuf);

impl ReportFile {
    fn new() -> Result<Self, CliError> {
        let name = fsio::unique_name("siderite-report", ".json");
        let path = std::env::temp_dir().join(name);
        // `create_new` refuses a file (or link) planted at the path.
        fsio::create_new(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// The output: any non-empty text, or exactly one JSON value when
    /// `json`.
    fn read(&self, json: bool) -> Option<String> {
        let text = std::fs::read_to_string(&self.0).ok()?;
        if json {
            serde_json::from_str::<serde_json::Value>(&text).ok()?;
        }
        (!text.is_empty()).then_some(text)
    }
}

impl Drop for ReportFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A `siderite` command that runs a Cargo subcommand in the app package,
/// forwarding every argument after the command unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Passthrough {
    /// The `siderite` command.
    pub command: &'static str,
    /// The Cargo subcommand it runs.
    pub cargo: &'static str,
    /// Whether `--bin` applies.
    pub takes_bin: bool,
    /// The rustup component providing the subcommand, if any.
    pub component: Option<&'static str>,
}

/// The Cargo passthroughs. `lint` is Clippy (Rust lints), unrelated to
/// the framework's `check`; `clean` only ever runs when invoked.
pub const PASSTHROUGHS: &[Passthrough] = &[
    Passthrough {
        command: "build",
        cargo: "build",
        takes_bin: true,
        component: None,
    },
    Passthrough {
        command: "test",
        cargo: "test",
        takes_bin: true,
        component: None,
    },
    Passthrough {
        command: "fmt",
        cargo: "fmt",
        takes_bin: false,
        component: Some("rustfmt"),
    },
    Passthrough {
        command: "lint",
        cargo: "clippy",
        takes_bin: true,
        component: Some("clippy"),
    },
    Passthrough {
        command: "clean",
        cargo: "clean",
        takes_bin: false,
        component: None,
    },
];

impl Passthrough {
    /// Reject `--bin` for subcommands without binaries.
    ///
    /// Errors:
    ///   A usage error when `bin` is set and `--bin` does not apply.
    pub fn check_bin(&self, bin: Option<&str>) -> Result<(), CliError> {
        if bin.is_some() && !self.takes_bin {
            let name = self.command;
            let msg = format!("--bin does not apply to `{name}`");
            return Err(CliError::usage(msg));
        }
        Ok(())
    }
}

/// The passthrough for `command`, if it is one.
pub fn passthrough(command: &str) -> Option<&'static Passthrough> {
    PASSTHROUGHS.iter().find(|p| p.command == command)
}

/// `cargo <sub>` in `package_dir` with the args after the `siderite`
/// command (no extra `--` unless the user passed some).
///
/// Errors:
///   A usage error for `--bin` on a subcommand without binaries, or an IO
///   error naming the rustup component when the subcommand is missing.
pub fn cargo_passthrough(
    package_dir: &Path,
    pass: &Passthrough,
    bin: Option<&str>,
    args: &[String],
) -> Result<u8, CliError> {
    pass.check_bin(bin)?;
    if let Some(component) = pass.component {
        require_subcommand(pass, component)?;
    }
    let rest = args_after(pass.command, args);
    let mut command = cargo_command(package_dir, pass.cargo, bin, &rest);
    exit_code(&mut command)
}

/// `cargo <sub>` is an external `cargo-<sub>` program; say how to get it
/// instead of letting cargo report an unknown command.
fn require_subcommand(pass: &Passthrough, component: &str) -> Done {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let program = format!("cargo-{}", pass.cargo);
    if crate::doctor::tools::which(&program, &path).is_some() {
        return Ok(());
    }
    Err(CliError::Io(format!(
        "`siderite {}` runs `cargo {}`, which needs {component}; install it \
         with `rustup component add {component}`",
        pass.command, pass.cargo
    )))
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
