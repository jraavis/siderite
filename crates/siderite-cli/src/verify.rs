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
//!
//! With `--json`, `lint` and `build` also pass `--message-format=json` and
//! their compiler messages become [`CompilerDiagnostic`]s (error codes,
//! source spans and rendered text; see [`DIAGNOSTICS_FORMAT`]). Each
//! message's rendered text is echoed to stderr as it arrives. Stable Rust
//! has no machine-readable libtest output, so `test` is reported only as an
//! aggregate pass/fail ([`TEST_RESULTS`]); terminal output is not parsed.

use crate::args::GlobalArgs;
use crate::envelope::CliEnvelope;
use crate::error::CliError;
use crate::project::{self, ResolvedProject, cargo_command};
use crate::toolchain::Probe;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

/// Version of the [`VerifyReport`] layout. Bumped on incompatible changes.
pub const VERIFY_FORMAT_VERSION: &str = "1";

/// Format of [`VerifyStep::diagnostics`]: cargo/rustc JSON diagnostics,
/// reduced as documented on [`CompilerDiagnostic`].
pub const DIAGNOSTICS_FORMAT: &str = "cargo-json-diagnostics/1";

/// Granularity of the `test` step: per-test results are unavailable on
/// stable Rust, so only the aggregate status and exit code are reported.
pub const TEST_RESULTS: &str = "aggregate";

/// Most diagnostics kept per step; the rest are counted in
/// [`VerifyStep::diagnostics_truncated`].
pub const MAX_DIAGNOSTICS: usize = 100;

/// A compiler or Clippy message from `cargo --message-format=json`.
///
/// Kept: level, error/lint code, message, spans, child notes and the
/// rendered text. Dropped on purpose: rustc's long `--explain` text (use
/// `rustc --explain CODE`), span source text and macro expansions.
/// Duplicates (one lint reported per target) are kept once, and span-less
/// summaries such as `aborting due to 2 previous errors` are omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompilerDiagnostic {
    /// `error`, `warning`, ...
    pub level: String,
    /// Error or lint code, e.g. `E0308` or `clippy::len_zero`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// One-line message.
    pub message: String,
    /// Source locations; the primary span is marked.
    pub spans: Vec<DiagnosticSpan>,
    /// Attached `note` / `help` messages, with their own spans.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ChildDiagnostic>,
    /// The diagnostic as rustc prints it in a terminal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rendered: Option<String>,
}

/// A `note` or `help` attached to a [`CompilerDiagnostic`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChildDiagnostic {
    /// `note`, `help`, ...
    pub level: String,
    /// Message text.
    pub message: String,
    /// Source locations, often a suggested replacement.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<DiagnosticSpan>,
}

/// A source location. `file` is as cargo reports it: relative to the
/// workspace root for workspace members, absolute otherwise. Lines and
/// columns are 1-based; end columns are exclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiagnosticSpan {
    /// Source file path.
    pub file: String,
    /// First line.
    pub line_start: u64,
    /// Last line.
    pub line_end: u64,
    /// First column.
    pub column_start: u64,
    /// Column after the last character.
    pub column_end: u64,
    /// Whether this is the main location of the diagnostic.
    pub is_primary: bool,
    /// Text rustc shows under the span.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Replacement text for a machine-applicable or suggested fix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_replacement: Option<String>,
}

#[derive(Deserialize)]
struct RawLine {
    reason: String,
    message: Option<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    level: String,
    message: String,
    code: Option<RawCode>,
    #[serde(default)]
    spans: Vec<RawSpan>,
    #[serde(default)]
    children: Vec<RawMessage>,
    rendered: Option<String>,
}

#[derive(Deserialize)]
struct RawCode {
    code: String,
}

#[derive(Deserialize)]
struct RawSpan {
    file_name: String,
    line_start: u64,
    line_end: u64,
    column_start: u64,
    column_end: u64,
    is_primary: bool,
    label: Option<String>,
    suggested_replacement: Option<String>,
}

impl From<RawSpan> for DiagnosticSpan {
    fn from(s: RawSpan) -> Self {
        Self {
            file: s.file_name,
            line_start: s.line_start,
            line_end: s.line_end,
            column_start: s.column_start,
            column_end: s.column_end,
            is_primary: s.is_primary,
            label: s.label,
            suggested_replacement: s.suggested_replacement,
        }
    }
}

/// Parse one line of `cargo --message-format=json` output. Other reasons
/// (artifacts, build-script output, `build-finished`), non-JSON lines and
/// span-less summaries return `None`.
fn parse_line(line: &str) -> Option<CompilerDiagnostic> {
    let raw: RawLine = serde_json::from_str(line).ok()?;
    if raw.reason != "compiler-message" {
        return None;
    }
    let msg = raw.message?;
    if !matches!(msg.level.as_str(), "error" | "warning")
        || (msg.spans.is_empty() && msg.message.starts_with("aborting due to"))
    {
        return None;
    }
    Some(CompilerDiagnostic {
        level: msg.level,
        code: msg.code.map(|c| c.code),
        message: msg.message,
        spans: msg.spans.into_iter().map(Into::into).collect(),
        children: msg
            .children
            .into_iter()
            .map(|c| ChildDiagnostic {
                level: c.level,
                message: c.message,
                spans: c.spans.into_iter().map(Into::into).collect(),
            })
            .collect(),
        rendered: msg.rendered,
    })
}

/// Collects a step's diagnostics: deduplicated, capped at
/// [`MAX_DIAGNOSTICS`].
#[derive(Default)]
struct Collector {
    seen: HashSet<String>,
    kept: Vec<CompilerDiagnostic>,
    truncated: usize,
}

impl Collector {
    /// Add one output line; returns the rendered text to echo, if new.
    fn push_line(&mut self, line: &str) -> Option<String> {
        let diag = parse_line(line)?;
        let key = diag
            .rendered
            .clone()
            .unwrap_or_else(|| format!("{}{:?}", diag.message, diag.spans));
        if !self.seen.insert(key) {
            return None;
        }
        let echo = diag.rendered.clone();
        if self.kept.len() < MAX_DIAGNOSTICS {
            self.kept.push(diag);
        } else {
            self.truncated += 1;
        }
        echo
    }
}

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
    /// For `lint` and `build` with `--json`, compiler diagnostics in
    /// [`DIAGNOSTICS_FORMAT`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<CompilerDiagnostic>,
    /// Diagnostics dropped beyond [`MAX_DIAGNOSTICS`].
    #[serde(skip_serializing_if = "is_zero")]
    pub diagnostics_truncated: usize,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The `data` of `verify --json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VerifyReport {
    /// [`VERIFY_FORMAT_VERSION`].
    pub format_version: &'static str,
    /// [`DIAGNOSTICS_FORMAT`].
    pub diagnostics_format: &'static str,
    /// [`TEST_RESULTS`].
    pub test_results: &'static str,
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
            format_version: VERIFY_FORMAT_VERSION,
            diagnostics_format: DIAGNOSTICS_FORMAT,
            test_results: TEST_RESULTS,
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
        // Structured diagnostics only where they are compiler output; on
        // `test` the flag would mix test-binary stdout into the stream.
        let structured = json && matches!(name, "lint" | "build");
        let args: Vec<String> = structured
            .then(|| "--message-format=json".to_owned())
            .into_iter()
            .chain(args.iter().map(|a| (*a).to_owned()))
            .collect();
        if !json {
            println!("==> {name}");
        }
        let step = run_step(probe, project, name, sub, &args, json, structured);
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
        diagnostics: Vec::new(),
        diagnostics_truncated: 0,
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
        diagnostics: Vec::new(),
        diagnostics_truncated: 0,
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
    structured: bool,
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
    let mut collector = Collector::default();
    let status = if structured {
        run_structured(&mut cmd, &mut collector)
    } else {
        if json {
            cmd.stdout(Stdio::from(io::stderr()));
        }
        cmd.status()
    };
    match status {
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
            diagnostics: collector.kept,
            diagnostics_truncated: collector.truncated,
        },
        Err(err) => failed(name, line, started, format!("cannot run cargo: {err}")),
    }
}

/// Run with stdout piped, collecting diagnostics line by line and echoing
/// their rendered text to stderr, where cargo's progress also goes.
fn run_structured(
    cmd: &mut Command,
    collector: &mut Collector,
) -> io::Result<std::process::ExitStatus> {
    let mut child = cmd.stdout(Stdio::piped()).spawn()?;
    if let Some(stdout) = child.stdout.take() {
        let mut err = io::stderr();
        // Byte lines, so non-UTF-8 output cannot stop the drain and leave
        // cargo blocked on a full pipe.
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        while matches!(reader.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
            if let Some(text) = collector.push_line(&String::from_utf8_lossy(&buf)) {
                let _ = err.write_all(text.as_bytes());
            }
            buf.clear();
        }
    }
    child.wait()
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
        stub_with(tag, clippy, build, run_out, "")
    }

    /// Like [`stub`], but `clippy` and `build` first print the real cargo
    /// JSON fixture `tests/fixtures/verify/<sub>.jsonl` when `fixtures` is
    /// set and the call asks for `--message-format=json`.
    fn stub_with(
        tag: &str,
        clippy: u8,
        build: u8,
        run_out: &str,
        fixtures: &str,
    ) -> (Probe, ResolvedProject) {
        let print = if fixtures.is_empty() {
            String::new()
        } else {
            format!(
                "case \"$*\" in *--message-format=json*) /bin/cat \"{fixtures}/$1.jsonl\"; \
                 echo 'not json'; echo '{{\"reason\":\"compiler-artifact\"}}';; esac\n"
            )
        };
        let script = format!(
            "case \"$2\" in --version) exit 0;; esac\n\
             case \"$1\" in clippy|build) {print} ;; esac\n\
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

    fn fixtures() -> String {
        format!("{}/tests/fixtures/verify", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn json_mode_collects_compiler_and_clippy_diagnostics() {
        let (probe, project) = stub_with("verify-diag", 1, 101, ENVELOPE, &fixtures());
        let report = verify(&probe, &project, &GlobalArgs::default(), true);
        let lint = &report.steps[1];
        // The flag belongs to cargo, so it must come before `--`.
        let flag = lint
            .command
            .iter()
            .position(|a| a == "--message-format=json");
        let dashes = lint.command.iter().position(|a| a == "--");
        assert!(flag.unwrap() < dashes.unwrap(), "{:?}", lint.command);
        assert_eq!(lint.diagnostics.len(), 2, "{:?}", lint.diagnostics);
        let d = &lint.diagnostics[0];
        assert_eq!(d.level, "error");
        assert_eq!(d.code.as_deref(), Some("clippy::len_zero"));
        assert!(d.rendered.as_ref().unwrap().contains("length comparison"));
        let span = &d.spans[0];
        assert_eq!((span.file.as_str(), span.line_start), ("src/main.rs", 1));
        assert!(span.is_primary);
        let fix = d.children.iter().flat_map(|c| &c.spans).next().unwrap();
        assert_eq!(fix.suggested_replacement.as_deref(), Some("v.is_empty()"));

        let build = &report.steps[2];
        assert_eq!(build.status, StepStatus::Failed);
        assert_eq!(build.diagnostics.len(), 1, "{:?}", build.diagnostics);
        let e = &build.diagnostics[0];
        assert_eq!(e.code.as_deref(), Some("E0308"));
        assert_eq!(e.spans.len(), 2);
        assert_eq!(e.spans.iter().filter(|s| s.is_primary).count(), 1);

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["format_version"], VERIFY_FORMAT_VERSION);
        assert_eq!(json["diagnostics_format"], DIAGNOSTICS_FORMAT);
        assert_eq!(json["test_results"], "aggregate");
        assert_eq!(
            json["steps"][2]["diagnostics"][0]["spans"][0]["file"],
            "src/main.rs"
        );
        assert!(json["steps"][0].get("diagnostics").is_none());
        assert!(json["steps"][2]["diagnostics"][0]["code"].is_string());
    }

    #[test]
    fn text_mode_keeps_the_plain_cargo_command() {
        let (probe, project) = stub_with("verify-text", 0, 0, ENVELOPE, &fixtures());
        let report = verify(&probe, &project, &GlobalArgs::default(), false);
        for step in &report.steps {
            assert!(
                !step
                    .command
                    .iter()
                    .any(|a| a.starts_with("--message-format"))
            );
            assert!(step.diagnostics.is_empty());
        }
    }

    #[test]
    fn test_step_never_requests_json_messages() {
        let (probe, project) = stub("verify-testflag", 0, 0, ENVELOPE);
        let report = verify(&probe, &project, &GlobalArgs::default(), true);
        assert!(
            !report.steps[3]
                .command
                .iter()
                .any(|a| a.starts_with("--message-format"))
        );
    }

    #[test]
    fn collector_dedupes_caps_and_skips_noise() {
        let line = |msg: &str| {
            serde_json::json!({"reason":"compiler-message","message":{
                "level":"warning","message":msg,"code":null,"spans":[],
                "children":[],"rendered":format!("warning: {msg}\n")}})
            .to_string()
        };
        let mut c = Collector::default();
        assert!(c.push_line(&line("same")).is_some());
        assert!(c.push_line(&line("same")).is_none());
        assert!(
            c.push_line(&line("aborting due to 2 previous errors"))
                .is_none()
        );
        assert!(c.push_line("garbage").is_none());
        assert!(
            c.push_line(r#"{"reason":"build-finished","success":true}"#)
                .is_none()
        );
        for i in 0..MAX_DIAGNOSTICS + 5 {
            c.push_line(&line(&format!("w{i}")));
        }
        assert_eq!(c.kept.len(), MAX_DIAGNOSTICS);
        assert_eq!(c.truncated, 5 + 1);
    }
}
