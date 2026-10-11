//! The container runtime: Docker or Podman, found explicitly.

use super::Failure;
use crate::doctor::tools::which;
use crate::report::redact_text;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

/// Variable naming the runtime program (`docker`, `podman` or a path).
pub(crate) const RUNTIME_ENV: &str = "SIDERITE_CONTAINER_RUNTIME";

/// Programs tried, in order, when [`RUNTIME_ENV`] is not set.
const KNOWN: [&str; 2] = ["docker", "podman"];

type Found<T> = Result<T, Failure>;
type Env<'a> = &'a [(&'a str, &'a str)];

/// Lines of the runtime's error output kept in a diagnostic.
const ERROR_LINES: usize = 5;

/// A container runtime program; Docker and Podman share the commands
/// used here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Runtime {
    /// The program to run.
    pub program: PathBuf,
}

/// The state of a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Presence {
    /// It runs.
    Running,
    /// It exists but does not run.
    Stopped,
    /// There is none.
    Absent,
}

impl Runtime {
    /// The runtime named by `chosen` (the value of [`RUNTIME_ENV`]), or
    /// the first of `docker` and `podman` on `path`.
    ///
    /// Errors:
    ///   `services.E001` when no runtime program is found.
    pub(crate) fn find(chosen: Option<&OsStr>, path: &OsStr) -> Found<Self> {
        let chosen = chosen.filter(|c| !c.is_empty());
        let found = match chosen {
            Some(name) => {
                let named = PathBuf::from(name);
                let direct = named.components().count() > 1;
                let name = name.to_string_lossy();
                if direct {
                    Some(named)
                } else {
                    which(&name, path)
                }
            }
            None => KNOWN.iter().find_map(|name| which(name, path)),
        };
        let missing = || Failure {
            code: "services.E001",
            message: "no container runtime found".to_owned(),
            hint: format!(
                "install Docker or Podman, or set {RUNTIME_ENV} to the \
                 runtime program"
            ),
        };
        let program = found.ok_or_else(missing)?;
        Ok(Self { program })
    }

    /// The runtime's file name, for reports.
    pub(crate) fn name(&self) -> String {
        let name = self.program.file_stem().unwrap_or_default();
        name.to_string_lossy().into_owned()
    }

    fn output(&self, args: &[&str], env: Env<'_>) -> Output {
        let mut command = Command::new(&self.program);
        command.args(args).stdin(Stdio::null());
        command.envs(env.iter().copied());
        command.output().unwrap_or_else(|err| Output {
            status: failed_status(),
            stdout: Vec::new(),
            stderr: err.to_string().into_bytes(),
        })
    }

    /// Run `args` with `env` added to the runtime's environment (values
    /// passed this way never appear on a command line).
    ///
    /// Errors:
    ///   `services.E002` with the end of the runtime's redacted error
    ///   output; the command line is not shown.
    pub(crate) fn run(&self, args: &[&str], env: Env<'_>) -> Found<String> {
        let output = self.output(args, env);
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            return Ok(text.trim().to_owned());
        }
        Err(self.failure(args, &output))
    }

    fn failure(&self, args: &[&str], output: &Output) -> Failure {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = stderr.lines().collect();
        let tail = lines[lines.len().saturating_sub(ERROR_LINES)..].join("\n");
        let verb = args.first().copied().unwrap_or_default();
        let name = self.name();
        Failure {
            code: "services.E002",
            message: format!("`{name} {verb}` failed: {}", redact_text(&tail)),
            hint: format!("check that the {name} daemon or service runs"),
        }
    }

    /// Whether the container `name` runs, is stopped or does not exist.
    ///
    /// Errors:
    ///   `services.E002` when the runtime cannot answer.
    pub(crate) fn container(&self, name: &str) -> Found<Presence> {
        let args = ["container", "inspect", "--format", "{{.State.Status}}"];
        let args = [&args[..], &[name]].concat();
        let output = self.output(&args, &[]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        match stdout.trim() {
            _ if is_missing(&output) => Ok(Presence::Absent),
            _ if !output.status.success() => Err(self.failure(&args, &output)),
            "running" => Ok(Presence::Running),
            _ => Ok(Presence::Stopped),
        }
    }

    /// Whether the volume `name` exists.
    ///
    /// Errors:
    ///   `services.E002` when the runtime cannot answer.
    pub(crate) fn volume(&self, name: &str) -> Found<bool> {
        let args = ["volume", "inspect", "--format", "{{.Name}}", name];
        let output = self.output(&args, &[]);
        if is_missing(&output) {
            return Ok(false);
        }
        if output.status.success() {
            return Ok(true);
        }
        Err(self.failure(&args, &output))
    }
}

/// Whether the runtime said the object does not exist.
fn is_missing(output: &Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    !output.status.success() && stderr.contains("no such")
}

#[cfg(unix)]
fn failed_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(1 << 8)
}

#[cfg(windows)]
fn failed_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(1)
}
