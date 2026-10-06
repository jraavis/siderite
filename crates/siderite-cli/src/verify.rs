//! `verify`: run the offline checks of a package in one command.
//!
//! The steps, in order, each through the same cargo runner as the
//! passthrough commands:
//!
//! | Step | Runs |
//! |---|---|
//! | `fmt` | `cargo fmt --check` (never rewrites files) |
//! | `lint` | `cargo clippy --all-targets -- -D warnings` (never `--fix`) |
//! | `build` | `cargo build --all-targets` |
//! | `test` | `cargo test` |
//! | `check` | `cargo run --quiet -- check --json` (framework checks) |
//!
//! Every step runs even when an earlier one fails, except that `test` and
//! `check` are skipped when `build` fails. Nothing connects to a database
//! and no migration is applied: the only profile is offline.

use crate::args::GlobalArgs;
use crate::envelope::CliEnvelope;
use crate::error::CliError;
use crate::project::{self, ResolvedProject, cargo_command};
use crate::toolchain::Probe;
use serde::Serialize;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

/// Outcome of one [`VerifyStep`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    /// The step succeeded.
    Passed,
    /// The step ran and failed, or could not run.
    Failed,
    /// The step did not run because a step it needs failed.
    Skipped,
}

impl StepStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// One step of a [`VerifyReport`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VerifyStep {
    /// Step name: `fmt`, `lint`, `build`, `test` or `check`.
    pub name: &'static str,
    /// The exact command line run, starting with `cargo`.
    pub command: Vec<String>,
    /// Outcome.
    pub status: StepStatus,
    /// Wall-clock time in milliseconds; `0` when skipped.
    pub duration_ms: u64,
    /// The child's exit code, when it ran and exited normally.
    pub exit_code: Option<i32>,
    /// Why the step failed or was skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// For `check`, the `data` of its JSON envelope (issues and counts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<serde_json::Value>,
}

/// The `data` of `verify --json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VerifyReport {
    /// Verification profile; always `offline`.
    pub profile: &'static str,
    /// Steps in execution order.
    pub steps: Vec<VerifyStep>,
    /// Number of passed steps.
    pub passed: usize,
    /// Number of failed steps.
    pub failed: usize,
    /// Number of skipped steps.
    pub skipped: usize,
}

impl VerifyReport {
    fn new(steps: Vec<VerifyStep>) -> Self {
        let count = |s: StepStatus| steps.iter().filter(|x| x.status == s).count();
        Self {
            profile: "offline",
            passed: count(StepStatus::Passed),
            failed: count(StepStatus::Failed),
            skipped: count(StepStatus::Skipped),
            steps,
        }
    }

    /// Whether every step passed.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.failed == 0 && self.skipped == 0
    }
}

/// `siderite verify [--json]` from `cwd`. Returns `0` when every step
/// passed, else `1`.
///
/// # Errors
/// Usage error for unexpected arguments or an ambiguous project.
pub fn run(cwd: &Path, global: &GlobalArgs, raw: &[String]) -> Result<u8, CliError> {
    let (_, rest) = crate::args::split_global(raw)?;
    if let Some(extra) = rest
        .iter()
        .filter(|a| *a != "--json")
        .find(|a| *a != "verify")
    {
        return Err(CliError::usage(format!(
            "unexpected argument `{extra}`; usage: siderite verify [--json]"
        )));
    }
    let resolved = project::resolve_project(cwd, global, "verify")?;
    let probe = Probe::system(&resolved.package_dir);
    let report = verify(&probe, &resolved, global, global.json);
    let code = u8::from(!report.ok());
    if global.json {
        let mut env = CliEnvelope::success("verify", report);
        env.ok = code == 0;
        let json = env
            .to_json_pretty()
            .map_err(|err| CliError::Io(format!("failed to serialize JSON: {err}")))?;
        println!("{json}");
    } else {
        print!("{}", render(&report));
    }
    Ok(code)
}

/// Run every step. With `json`, child stdout goes to stderr so stdout
/// stays reserved for the report.
pub(crate) fn verify(
    probe: &Probe,
    project: &ResolvedProject,
    global: &GlobalArgs,
    json: bool,
) -> VerifyReport {
    let plan: [(&'static str, &str, &[&str]); 4] = [
        ("fmt", "fmt", &["--check"]),
        ("lint", "clippy", &["--all-targets", "--", "-D", "warnings"]),
        ("build", "build", &["--all-targets"]),
        ("test", "test", &[]),
    ];
    let mut steps = Vec::new();
    let mut built = true;
    for (name, sub, args) in plan {
        if name == "test" && !built {
            steps.push(skipped(name, "build failed"));
            continue;
        }
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        if !json {
            println!("==> {name}");
        }
        let step = run_step(probe, project, name, sub, &args, json);
        if name == "build" {
            built = step.status == StepStatus::Passed;
        }
        steps.push(step);
    }
    if built {
        if !json {
            println!("==> check");
        }
        steps.push(check_step(probe, project, global));
    } else {
        steps.push(skipped("check", "build failed"));
    }
    VerifyReport::new(steps)
}

fn skipped(name: &'static str, why: &str) -> VerifyStep {
    VerifyStep {
        name,
        command: Vec::new(),
        status: StepStatus::Skipped,
        duration_ms: 0,
        exit_code: None,
        message: Some(why.to_owned()),
        report: None,
    }
}

fn command_line(cmd: &Command) -> Vec<String> {
    std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

fn failed(name: &'static str, command: Vec<String>, started: Instant, why: String) -> VerifyStep {
    VerifyStep {
        name,
        command,
        status: StepStatus::Failed,
        duration_ms: elapsed_ms(started),
        exit_code: None,
        message: Some(why),
        report: None,
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn run_step(
    probe: &Probe,
    project: &ResolvedProject,
    name: &'static str,
    sub: &str,
    args: &[String],
    json: bool,
) -> VerifyStep {
    let started = Instant::now();
    let mut cmd = match cargo_command(probe, project, sub, args) {
        Ok(cmd) => cmd,
        Err(err) => {
            let line = std::iter::once(format!("cargo {sub}"))
                .chain(args.iter().cloned())
                .collect();
            return failed(name, line, started, err.to_string());
        }
    };
    let line = command_line(&cmd);
    if json {
        cmd.stdout(Stdio::from(io::stderr()));
    }
    match cmd.status() {
        Ok(status) => VerifyStep {
            name,
            command: line,
            status: if status.success() {
                StepStatus::Passed
            } else {
                StepStatus::Failed
            },
            duration_ms: elapsed_ms(started),
            exit_code: status.code(),
            message: (!status.success()).then(|| format!("cargo {sub} failed")),
            report: None,
        },
        Err(err) => failed(name, line, started, format!("cannot run cargo: {err}")),
    }
}

/// `check --json` through the app binary. Stdout must be exactly one
/// envelope; anything else fails the step instead of being guessed at.
fn check_step(probe: &Probe, project: &ResolvedProject, global: &GlobalArgs) -> VerifyStep {
    let started = Instant::now();
    let mut args = vec!["--quiet".to_owned()];
    if let Some(bin) = &project.binary_name {
        args.extend(["--bin".to_owned(), bin.clone()]);
    }
    args.extend(["--".to_owned(), "check".to_owned(), "--json".to_owned()]);
    if let Some(dir) = &global.migrations_dir {
        args.push(format!("--migrations-dir={}", dir.display()));
    }
    let mut cmd = match cargo_command(probe, project, "run", &args) {
        Ok(cmd) => cmd,
        Err(err) => return failed("check", Vec::new(), started, err.to_string()),
    };
    let line = command_line(&cmd);
    let output = match cmd.stdout(Stdio::piped()).output() {
        Ok(out) => out,
        Err(err) => return failed("check", line, started, format!("cannot run cargo: {err}")),
    };
    let _ = io::stderr().write_all(&output.stderr);
    let mut step = failed("check", line, started, String::new());
    step.exit_code = output.status.code();
    let Ok(env) = serde_json::from_slice::<CliEnvelope<serde_json::Value>>(&output.stdout) else {
        step.message = Some(
            "stdout is not one check envelope (does the app factory print to \
             stdout, or does the binary not use AppCli?)"
                .to_owned(),
        );
        return step;
    };
    if env.command != "check" {
        step.message = Some(format!("unexpected envelope for `{}`", env.command));
        return step;
    }
    let passed = env.ok && output.status.success();
    step.status = if passed {
        StepStatus::Passed
    } else {
        StepStatus::Failed
    };
    step.message = (!passed).then(|| {
        env.diagnostics.first().map_or_else(
            || "framework check found errors".to_owned(),
            |d| d.message.clone(),
        )
    });
    step.report = env.data;
    step
}

/// Text summary: one line per step, then check issues and a total.
fn render(report: &VerifyReport) -> String {
    let mut out = format!("\nverify ({})\n", report.profile);
    for step in &report.steps {
        let detail = match (&step.message, step.exit_code) {
            (Some(msg), Some(code)) => format!("{msg} (exit {code})"),
            (Some(msg), None) => msg.clone(),
            (None, _) => String::new(),
        };
        let time = if step.status == StepStatus::Skipped {
            String::new()
        } else {
            format!("{:.1}s", step.duration_ms as f64 / 1000.0)
        };
        let line = format!(
            "  {:<8} {:<6} {:>7}  {detail}",
            step.status.label(),
            step.name,
            time
        );
        out.push_str(line.trim_end());
        out.push('\n');
        for issue in step
            .report
            .as_ref()
            .and_then(|r| r["issues"].as_array())
            .into_iter()
            .flatten()
        {
            out.push_str(&format!(
                "           {}: [{}] {}\n",
                issue["level"].as_str().unwrap_or("?"),
                issue["id"].as_str().unwrap_or("?"),
                issue["message"].as_str().unwrap_or("")
            ));
        }
    }
    out.push_str(&format!(
        "{} passed, {} failed, {} skipped\n",
        report.passed, report.failed, report.skipped
    ));
    out
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::toolchain::tests::stub_bin;

    const ENVELOPE: &str = r#"{"schema_version":"1.0","command":"check","ok":true,"data":{"issues":[],"errors":0,"warnings":0},"diagnostics":[]}"#;

    /// A stub cargo whose subcommands exit with the given codes; `run`
    /// prints `run_out`.
    fn stub(tag: &str, clippy: u8, build: u8, run_out: &str) -> (Probe, ResolvedProject) {
        let script = format!(
            "case \"$2\" in --version) exit 0;; esac\n\
             case \"$1\" in\n\
             clippy) exit {clippy};;\n\
             build) exit {build};;\n\
             run) printf '%s' '{run_out}'; exit 0;;\n\
             *) exit 0;;\n\
             esac"
        );
        let bin = stub_bin(tag, &[("cargo", &script)]);
        let project = ResolvedProject {
            package_dir: bin.clone(),
            manifest_path: Some(bin.join("Cargo.toml")),
            package_name: Some("demo".into()),
            binary_name: Some("demo".into()),
        };
        (Probe::with_path(&bin, &bin), project)
    }

    fn statuses(report: &VerifyReport) -> Vec<(&str, StepStatus)> {
        report.steps.iter().map(|s| (s.name, s.status)).collect()
    }

    #[test]
    fn all_steps_pass() {
        let (probe, project) = stub("verify-ok", 0, 0, ENVELOPE);
        let report = verify(&probe, &project, &GlobalArgs::default(), true);
        assert!(report.ok(), "{report:?}");
        assert_eq!(report.passed, 5);
        let check = report.steps.last().unwrap();
        assert_eq!(check.report.as_ref().unwrap()["errors"], 0);
        assert!(check.command.contains(&"--quiet".to_owned()));
        assert!(check.command.ends_with(&["check".into(), "--json".into()]));
        let fmt = &report.steps[0];
        assert_eq!(fmt.command[0], "cargo");
        assert!(fmt.command.ends_with(&["--check".into()]));
    }

    #[test]
    fn later_steps_run_after_a_lint_failure() {
        let (probe, project) = stub("verify-lint", 1, 0, ENVELOPE);
        let report = verify(&probe, &project, &GlobalArgs::default(), true);
        assert!(!report.ok());
        assert_eq!(
            statuses(&report),
            [
                ("fmt", StepStatus::Passed),
                ("lint", StepStatus::Failed),
                ("build", StepStatus::Passed),
                ("test", StepStatus::Passed),
                ("check", StepStatus::Passed),
            ]
        );
        assert_eq!(report.steps[1].exit_code, Some(1));
    }

    #[test]
    fn a_build_failure_skips_test_and_check() {
        let (probe, project) = stub("verify-build", 0, 101, ENVELOPE);
        let report = verify(&probe, &project, &GlobalArgs::default(), true);
        assert_eq!(report.failed, 1);
        assert_eq!(report.skipped, 2);
        assert_eq!(report.steps[3].status, StepStatus::Skipped);
        assert_eq!(report.steps[4].status, StepStatus::Skipped);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["steps"][4]["status"], "skipped");
    }

    #[test]
    fn contaminated_check_output_fails_the_step() {
        let (probe, project) = stub("verify-dirty", 0, 0, "hello\n{}");
        let report = verify(&probe, &project, &GlobalArgs::default(), true);
        let check = report.steps.last().unwrap();
        assert_eq!(check.status, StepStatus::Failed);
        assert!(check.message.as_ref().unwrap().contains("stdout"));
        assert!(check.report.is_none());
    }

    #[test]
    fn check_errors_fail_the_step_and_are_rendered() {
        let failing = r#"{"schema_version":"1.0","command":"check","ok":false,"data":{"issues":[{"level":"error","id":"models.E003","message":"no pk"}],"errors":1,"warnings":0},"diagnostics":[]}"#;
        let (probe, project) = stub("verify-check", 0, 0, failing);
        let report = verify(&probe, &project, &GlobalArgs::default(), true);
        assert_eq!(report.steps[4].status, StepStatus::Failed);
        let text = render(&report);
        assert!(text.contains("error: [models.E003] no pk"), "{text}");
        assert!(text.contains("4 passed, 1 failed, 0 skipped"), "{text}");
    }
}
