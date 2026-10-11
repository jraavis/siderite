//! `siderite services up|down|status|env|destroy`: optional local
//! development services, run by an explicitly found container runtime
//! (Docker or Podman). PostgreSQL is the one recipe.
//!
//! Resources are scoped to the package: names carry the directory's name
//! and a random id recorded in `.siderite/local/` (git-ignored, owner
//! only) with the generated credentials and the local port. Ports are
//! published on 127.0.0.1 only. `down` keeps the data volume; only
//! `destroy postgres --yes` deletes data.

use crate::error::CliError;
use crate::report::text::diagnostic_line;
use crate::report::{Diagnostic, Envelope, Severity, channel};
use crate::target::{self, ProjectArgs};
use serde::Serialize;
use std::fmt::Write;
use std::path::Path;
use std::time::Duration;

mod postgres;
mod runtime;
mod state;
#[cfg(test)]
mod tests;

use postgres::Wanted;
use runtime::{RUNTIME_ENV, Runtime};

const USAGE: &str = "usage: services up [postgres] [--port N] \
                     [--image IMAGE] [--timeout SECS] [--json] | \
                     services down|status [--json] | services env | \
                     services destroy postgres --yes [--json]";

/// Seconds `up` waits for the database by default.
const DEFAULT_TIMEOUT: u64 = 60;

/// A step that failed, as a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    /// The diagnostic code.
    pub code: &'static str,
    /// What went wrong (redacted).
    pub message: String,
    /// What to do about it.
    pub hint: String,
}

impl Failure {
    fn diagnostic(&self) -> Diagnostic {
        let diag = Diagnostic::new(self.code, Severity::Error, &self.message);
        diag.with_hint(&self.hint)
    }
}

/// The subcommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Start, waiting until ready.
    Up,
    /// Stop, keeping data.
    Down,
    /// Report.
    Status,
    /// Print `DATABASE_URL`.
    Env,
    /// Delete the container, the data and the state.
    Destroy,
}

impl Action {
    const fn command(self) -> &'static str {
        match self {
            Self::Up => "services up",
            Self::Down => "services down",
            Self::Status => "services status",
            Self::Env => "services env",
            Self::Destroy => "services destroy",
        }
    }
}

/// Parsed arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    /// The subcommand.
    pub action: Action,
    /// `--json`.
    pub json: bool,
    /// `--port` and `--image` (`up` only).
    pub wanted: Wanted,
    /// `--timeout` (`up` only).
    pub timeout: Duration,
}

fn action(word: Option<&str>) -> Result<Action, CliError> {
    match word {
        Some("up") => Ok(Action::Up),
        Some("down") => Ok(Action::Down),
        Some("status") => Ok(Action::Status),
        Some("env") => Ok(Action::Env),
        Some("destroy") => Ok(Action::Destroy),
        _ => Err(CliError::usage(USAGE)),
    }
}

type Usage<T> = Result<T, CliError>;
type Step = Result<(), Failure>;
type Words<'a, 'b> = &'b mut dyn Iterator<Item = &'a String>;

fn value<'a>(flag: &str, args: Words<'a, '_>) -> Usage<&'a str> {
    let missing = || CliError::usage(format!("{flag} needs a value"));
    args.next().map(String::as_str).ok_or_else(missing)
}

fn number<T: std::str::FromStr>(flag: &str, text: &str) -> Usage<T> {
    let bad = || CliError::usage(format!("{flag} takes a positive number"));
    text.parse().map_err(|_| bad())
}

/// Parse the arguments after the project flags (`services ...`).
///
/// Errors:
///   A usage error for an unknown subcommand, service or flag, a flag
///   that does not apply to the subcommand, or `destroy` without both
///   `postgres` and `--yes`.
pub(crate) fn parse(raw: &[String]) -> Result<Options, CliError> {
    let mut args = raw.iter().skip_while(|a| *a != "services").skip(1);
    let action = action(args.next().map(String::as_str))?;
    let mut options = Options {
        action,
        json: false,
        wanted: Wanted::default(),
        timeout: Duration::from_secs(DEFAULT_TIMEOUT),
    };
    let (mut named, mut yes) = (false, false);
    let up = action == Action::Up;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" if action != Action::Env => options.json = true,
            "--yes" if action == Action::Destroy => yes = true,
            postgres::NAME => named = true,
            "--port" if up => {
                let port = number("--port", value("--port", &mut args)?)?;
                options.wanted.port = Some(port).filter(|p| *p != 0);
            }
            "--image" if up => {
                let image = value("--image", &mut args)?;
                options.wanted.image = Some(image.to_owned());
            }
            "--timeout" if up => {
                let text = value("--timeout", &mut args)?;
                let secs = number::<u64>("--timeout", text)?.max(1);
                options.timeout = Duration::from_secs(secs);
            }
            other => {
                let msg = format!("unexpected argument `{other}`; {USAGE}");
                return Err(CliError::usage(msg));
            }
        }
    }
    if action == Action::Destroy && !(named && yes) {
        return Err(CliError::usage(
            "destroy deletes the database data: run \
             `siderite services destroy postgres --yes`",
        ));
    }
    Ok(options)
}

/// One service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ServiceReport {
    /// The recipe.
    pub name: &'static str,
    /// The container's state.
    pub status: Status,
    /// The container.
    pub container: String,
    /// The data volume.
    pub volume: String,
    /// Whether the data volume exists.
    pub data_volume: bool,
    /// The image.
    pub image: String,
    /// Where it listens on this machine.
    pub address: String,
    /// The connection URL without its password.
    pub url: String,
}

/// A container's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    /// It runs.
    Running,
    /// It exists but does not run.
    Stopped,
    /// There is no container.
    Absent,
}

/// The `data` of a `services` envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ServicesData {
    /// The runtime used, when one was found.
    pub runtime: Option<String>,
    /// The services recorded for the package (empty when none, or after
    /// `destroy`).
    pub services: Vec<ServiceReport>,
}

type Report = Envelope<ServicesData>;
type Exit = Usage<u8>;

/// Carry out `options` in the package `dir` with `runtime`.
///
/// Args:
///   options: The parsed arguments (not `env`).
///   runtime: The runtime, or why none was found.
///   dir: The package directory.
///
/// Returns:
///   The report; runtime problems are diagnostics in it.
///
/// Errors:
///   IO errors on the recorded state.
pub(crate) fn execute(
    options: &Options,
    runtime: Result<Runtime, Failure>,
    dir: &Path,
) -> Result<Report, CliError> {
    let command = options.action.command();
    let mut data = ServicesData {
        runtime: None,
        services: Vec::new(),
    };
    let runtime = match runtime {
        Ok(runtime) => runtime,
        Err(failure) => {
            let diags = vec![failure.diagnostic()];
            return Ok(Envelope::new(command, data, diags));
        }
    };
    data.runtime = Some(runtime.name());
    let recorded = match options.action {
        Action::Up => Some(postgres::prepare(dir, &options.wanted)?),
        _ => state::load(dir)?,
    };
    let Some(recorded) = recorded else {
        return Ok(Envelope::new(command, data, Vec::new()));
    };
    let service = postgres::Service::new(&runtime, dir, &recorded);
    let done = apply(options, &service).and_then(|()| {
        if options.action == Action::Destroy {
            return Ok(None);
        }
        service.report().map(Some)
    });
    let diags = match done {
        Ok(found) => {
            data.services.extend(found);
            Vec::new()
        }
        Err(failure) => vec![failure.diagnostic()],
    };
    if options.action == Action::Destroy && diags.is_empty() {
        state::remove(dir)?;
    }
    Ok(Envelope::new(command, data, diags))
}

fn apply(options: &Options, service: &postgres::Service<'_>) -> Step {
    match options.action {
        Action::Up => service.up(options.timeout),
        Action::Down => service.down(),
        Action::Destroy => service.destroy(),
        Action::Status | Action::Env => Ok(()),
    }
}

/// Text form: the runtime, each service and the diagnostics.
pub(crate) fn render(report: &Report, action: Action) -> String {
    let data = &report.data;
    let mut out = String::new();
    if let Some(runtime) = &data.runtime {
        let _ = writeln!(out, "runtime: {runtime}");
    }
    for s in &data.services {
        let status = format!("{:?}", s.status).to_lowercase();
        let kept = if s.data_volume { "" } else { " (absent)" };
        let (name, address) = (s.name, &s.address);
        let _ = writeln!(out, "{name}  {status}  {address}  {}", s.image);
        let _ = writeln!(out, "  container  {}", s.container);
        let _ = writeln!(out, "  volume     {}{kept}", s.volume);
        let _ = writeln!(out, "  url        {}", s.url);
    }
    let quiet = !report.ok || !data.services.is_empty();
    match action {
        Action::Destroy if report.ok => {
            let done = "postgres: container, data and state removed";
            let _ = writeln!(out, "{done}");
        }
        Action::Up if report.ok => {
            let _ = writeln!(out, "DATABASE_URL: run `siderite services env`");
        }
        _ if !quiet => {
            let none = "no services here; run `siderite services up`";
            let _ = writeln!(out, "{none}");
        }
        _ => {}
    }
    for diag in &report.diagnostics {
        let _ = writeln!(out, "{}", diagnostic_line(diag));
        if let Some(hint) = diag.hint() {
            let _ = writeln!(out, "  hint: {hint}");
        }
    }
    out
}

/// `services env`: `DATABASE_URL=...` with the password, for the shell
/// or a `.env` file.
fn env(dir: &Path) -> Usage<u8> {
    let Some(recorded) = state::load(dir)? else {
        return Err(CliError::usage(
            "no postgres service here; run `siderite services up` first",
        ));
    };
    println!("DATABASE_URL={}", postgres::url(&recorded));
    Ok(0)
}

/// `siderite services` from `cwd`.
///
/// Errors:
///   Usage errors, IO errors on the recorded state, or an IO error
///   writing the report.
pub(crate) fn run(raw: &[String], cwd: &Path, project: &ProjectArgs) -> Exit {
    let options = parse(raw)?;
    let dir = target::package_dir(cwd, project)?;
    if options.action == Action::Env {
        return env(&dir);
    }
    let chosen = std::env::var_os(RUNTIME_ENV);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let runtime = Runtime::find(chosen.as_deref(), &path);
    let report = execute(&options, runtime, &dir)?;
    if options.json {
        channel::emit_json(&report, None)?;
    } else {
        print!("{}", render(&report, options.action));
    }
    Ok(report.exit_code())
}
