//! Offline toolchain probing shared by `doctor` and `setup`.
//!
//! Probes only run version queries. Every `rustc`, `cargo` and `rustup` child
//! gets `RUSTUP_AUTO_INSTALL=0` so a `rust-toolchain.toml` override never
//! triggers a download, and stdin is closed so nothing can prompt.

use serde::Serialize;
use std::cmp::Ordering;
use std::env;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Outcome of one diagnostic check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    /// The prerequisite is present and compatible.
    Pass,
    /// Advisory problem; the command still succeeds.
    Warn,
    /// A required prerequisite is missing or incompatible.
    Fail,
    /// Not checked, with the reason in the message.
    Skip,
}

impl CheckStatus {
    /// Lowercase label used in text output.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Skip => "skip",
        }
    }
}

/// One diagnostic result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// Stable dotted identifier, e.g. `toolchain.rustc`.
    pub id: String,
    /// Outcome.
    pub status: CheckStatus,
    /// What was found.
    pub message: String,
    /// Exact next step, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Check {
    pub(crate) fn new(id: &str, status: CheckStatus, message: impl Into<String>) -> Self {
        Self {
            id: id.to_owned(),
            status,
            message: message.into(),
            hint: None,
        }
    }

    pub(crate) fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// Render checks as aligned text lines.
#[must_use]
pub fn render_checks(checks: &[Check]) -> String {
    let width = checks.iter().map(|c| c.id.len()).max().unwrap_or(0);
    let mut out = String::new();
    for check in checks {
        out.push_str(&format!(
            "[{}] {:<width$}  {}\n",
            check.status.label(),
            check.id,
            check.message
        ));
        if let Some(hint) = &check.hint {
            out.push_str(&format!("       {:<width$}  -> {hint}\n", ""));
        }
    }
    out
}

/// A `major.minor.patch` Rust version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct RustVersion {
    /// Major version.
    pub major: u64,
    /// Minor version.
    pub minor: u64,
    /// Patch version (0 when omitted, as in `rust-version = "1.99"`).
    pub patch: u64,
}

impl RustVersion {
    /// Parse `1.99`, `1.99.0` or `1.99.0-nightly`; pre-release tags are ignored.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let core = text.trim().split(['-', '+', ' ']).next()?;
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = match parts.next() {
            Some(p) => p.parse().ok()?,
            None => 0,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for RustVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Runs version queries, optionally against an explicit `PATH`.
#[derive(Debug, Clone)]
pub struct Probe {
    cwd: PathBuf,
    path: Option<OsString>,
    rustc: OsString,
    cargo: OsString,
}

impl Probe {
    /// Probe the real environment from `cwd`, honoring `RUSTC` and `CARGO`.
    #[must_use]
    pub fn system(cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            path: None,
            rustc: env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()),
            cargo: env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
        }
    }

    /// Probe with an explicit `PATH` and plain `rustc` / `cargo` names.
    #[must_use]
    pub fn with_path(cwd: &Path, path: impl Into<OsString>) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            path: Some(path.into()),
            rustc: "rustc".into(),
            cargo: "cargo".into(),
        }
    }

    /// The same probe run from another directory.
    #[must_use]
    pub fn in_dir(&self, cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            ..self.clone()
        }
    }

    pub(crate) fn command(&self, program: impl Into<OsString>) -> Command {
        let mut cmd = Command::new(program.into());
        cmd.current_dir(&self.cwd)
            .env("RUSTUP_AUTO_INSTALL", "0")
            .stdin(Stdio::null());
        if let Some(path) = &self.path {
            cmd.env("PATH", path);
        }
        cmd
    }

    pub(crate) fn cargo(&self) -> Command {
        self.command(self.cargo.clone())
    }

    /// Trimmed stdout of a successful run, `None` if it fails or is missing.
    fn output(&self, program: impl Into<OsString>, args: &[&str]) -> Option<String> {
        match self.run(program, args) {
            Run::Ok(out) => Some(out),
            Run::Missing | Run::Failed(_) => None,
        }
    }

    /// Run `program`, telling a missing program from one that fails.
    fn run(&self, program: impl Into<OsString>, args: &[&str]) -> Run {
        match self.command(program).args(args).output() {
            Err(_) => Run::Missing,
            Ok(out) if out.status.success() => {
                Run::Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
            }
            Ok(out) => Run::Failed(first_line(&String::from_utf8_lossy(&out.stderr))),
        }
    }

    /// Whether `program` is an executable file on the probe's `PATH`.
    #[must_use]
    pub fn on_path(&self, program: &str) -> bool {
        let Some(path) = self.path.clone().or_else(|| env::var_os("PATH")) else {
            return false;
        };
        env::split_paths(&path).any(|dir| {
            let candidate = dir.join(program);
            candidate.is_file() || (cfg!(windows) && candidate.with_extension("exe").is_file())
        })
    }
}

/// Outcome of running a tool.
enum Run {
    Ok(String),
    Missing,
    /// Non-zero exit, with the first line of stderr.
    Failed(String),
}

pub(crate) fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_owned()
}

/// Next step for a tool that exists but exits with `stderr_line`.
pub(crate) fn failure_hint(stderr_line: &str) -> Option<&'static str> {
    // rustup proxies fail this way when a toolchain file pins a toolchain
    // that is not installed (auto-install is disabled for probes).
    stderr_line
        .contains("is not installed")
        .then_some("rustup toolchain install")
}

fn failed_check(id: &str, tool: &str, stderr_line: &str) -> Check {
    let check = Check::new(
        id,
        CheckStatus::Fail,
        if stderr_line.is_empty() {
            format!("{tool} is on PATH but failed to run")
        } else {
            format!("{tool} failed: {stderr_line}")
        },
    );
    match failure_hint(stderr_line) {
        Some(hint) => check.hint(hint),
        None => check,
    }
}

/// Facts gathered while checking the toolchain.
#[derive(Debug, Clone, Default)]
pub struct ToolchainFacts {
    /// `rustc` release, when it could be read.
    pub rustc: Option<RustVersion>,
    /// Host target triple reported by `rustc -vV`.
    pub host: Option<String>,
    /// Whether `rustup` is on `PATH`.
    pub rustup: bool,
}

/// Minimum version a check compares against, and where it comes from.
#[derive(Debug, Clone)]
pub struct Requirement {
    /// Minimum supported Rust version.
    pub version: RustVersion,
    /// Human-readable source, e.g. "package `shop` rust-version".
    pub source: String,
}

/// Command that installs Rust through rustup on this platform.
#[must_use]
pub fn rustup_install_hint() -> &'static str {
    if cfg!(windows) {
        "download and run rustup-init.exe from https://rustup.rs"
    } else {
        "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    }
}

fn rustc_vv(output: &str) -> (Option<RustVersion>, Option<String>) {
    let field = |key: &str| {
        output
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_owned())
    };
    let release = field("release:").or_else(|| {
        output
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .map(str::to_owned)
    });
    (
        release.as_deref().and_then(RustVersion::parse),
        field("host:"),
    )
}

/// Check `rustc`, `cargo`, `rustup`, the minimum version and a linker.
#[must_use]
pub fn toolchain_checks(
    probe: &Probe,
    required: Option<&Requirement>,
) -> (Vec<Check>, ToolchainFacts) {
    let mut checks = Vec::new();
    let mut facts = ToolchainFacts {
        rustup: probe.on_path("rustup"),
        ..ToolchainFacts::default()
    };

    let rustc_out = probe.run(probe.rustc.clone(), &["-vV"]);
    let rustc_runs = !matches!(rustc_out, Run::Missing);
    match &rustc_out {
        Run::Missing => checks.push(
            Check::new(
                "toolchain.rustc",
                CheckStatus::Fail,
                "rustc not found on PATH",
            )
            .hint(rustup_install_hint()),
        ),
        Run::Failed(err) => checks.push(failed_check("toolchain.rustc", "rustc", err)),
        Run::Ok(out) => {
            let (version, host) = rustc_vv(out);
            facts.rustc = version;
            facts.host = host;
            checks.push(match version {
                Some(v) => {
                    let host = facts.host.as_deref().unwrap_or("unknown host");
                    Check::new(
                        "toolchain.rustc",
                        CheckStatus::Pass,
                        format!("rustc {v} ({host})"),
                    )
                }
                None => Check::new(
                    "toolchain.rustc",
                    CheckStatus::Warn,
                    "rustc runs but its version could not be read",
                ),
            });
        }
    }

    checks.push(match probe.run(probe.cargo.clone(), &["--version"]) {
        Run::Ok(out) => Check::new("toolchain.cargo", CheckStatus::Pass, out),
        Run::Failed(err) => failed_check("toolchain.cargo", "cargo", &err),
        Run::Missing => Check::new(
            "toolchain.cargo",
            CheckStatus::Fail,
            "cargo not found on PATH",
        )
        .hint(rustup_install_hint()),
    });

    checks.push(if facts.rustup {
        Check::new(
            "toolchain.rustup",
            CheckStatus::Pass,
            "rustup manages the toolchain",
        )
    } else if rustc_runs {
        Check::new(
            "toolchain.rustup",
            CheckStatus::Warn,
            "rustup not found; the toolchain is not managed by rustup",
        )
        .hint("update Rust through the tool that installed it, or switch to rustup")
    } else {
        Check::new("toolchain.rustup", CheckStatus::Skip, "rustup not found")
    });

    checks.push(msrv_check(facts.rustc, required, facts.rustup));
    checks.push(linker_check(probe));
    (checks, facts)
}

fn msrv_check(actual: Option<RustVersion>, required: Option<&Requirement>, rustup: bool) -> Check {
    let id = "toolchain.version";
    let Some(req) = required else {
        return Check::new(id, CheckStatus::Skip, "no minimum Rust version declared");
    };
    let Some(actual) = actual else {
        return Check::new(id, CheckStatus::Skip, "rustc version unknown");
    };
    match actual.cmp(&req.version) {
        Ordering::Less => Check::new(
            id,
            CheckStatus::Fail,
            format!(
                "rustc {actual} is older than {} required by {}",
                req.version, req.source
            ),
        )
        .hint(if rustup {
            "rustup update stable".to_owned()
        } else {
            format!("install Rust {} or newer", req.version)
        }),
        _ => Check::new(
            id,
            CheckStatus::Pass,
            format!("rustc {actual} meets {} ({})", req.version, req.source),
        ),
    }
}

fn linker_check(probe: &Probe) -> Check {
    let id = "toolchain.linker";
    if cfg!(windows) {
        return Check::new(id, CheckStatus::Skip, "linker not checked on Windows")
            .hint("if linking fails, install Visual Studio Build Tools with the C++ workload");
    }
    // On macOS `/usr/bin/cc` is a shim that opens an installer dialog when the
    // Command Line Tools are missing, so ask xcode-select first.
    if cfg!(target_os = "macos") && probe.output("xcode-select", &["-p"]).is_none() {
        return Check::new(
            id,
            CheckStatus::Fail,
            "Xcode Command Line Tools not installed",
        )
        .hint("xcode-select --install");
    }
    match probe.output("cc", &["--version"]) {
        Some(out) => {
            let first = out.lines().next().unwrap_or("cc").to_owned();
            Check::new(id, CheckStatus::Pass, format!("cc: {first}"))
        }
        None => {
            let hint = if cfg!(target_os = "macos") {
                "xcode-select --install".to_owned()
            } else {
                linux_linker_hint(&std::fs::read_to_string("/etc/os-release").unwrap_or_default())
                    .to_owned()
            };
            Check::new(id, CheckStatus::Fail, "no C linker (`cc`) found").hint(hint)
        }
    }
}

/// Package-manager command installing a C toolchain, from `/etc/os-release`.
#[must_use]
pub fn linux_linker_hint(os_release: &str) -> &'static str {
    let ids: Vec<&str> = os_release
        .lines()
        .filter_map(|l| l.strip_prefix("ID=").or_else(|| l.strip_prefix("ID_LIKE=")))
        .flat_map(|v| v.trim_matches('"').split_whitespace())
        .collect();
    let has = |names: &[&str]| ids.iter().any(|id| names.contains(id));
    if has(&["debian", "ubuntu"]) {
        "sudo apt install build-essential"
    } else if has(&["fedora", "rhel", "centos"]) {
        "sudo dnf install gcc"
    } else if has(&["arch"]) {
        "sudo pacman -S base-devel"
    } else if has(&["alpine"]) {
        "sudo apk add build-base"
    } else if has(&["opensuse", "suse"]) {
        "sudo zypper install gcc"
    } else {
        "install gcc or clang with your system package manager"
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn v(text: &str) -> RustVersion {
        RustVersion::parse(text).unwrap()
    }

    #[test]
    fn parses_and_orders_versions() {
        assert_eq!(v("1.99"), v("1.99.0"));
        assert!(v("1.100.0") > v("1.99"));
        assert_eq!(v("1.99.0-nightly"), v("1.99.0"));
        assert_eq!(v("1.99.1 (abc 2026-01-01)"), v("1.99.1"));
        assert!(RustVersion::parse("garbage").is_none());
        assert!(RustVersion::parse("1").is_none());
        assert!(RustVersion::parse("1.2.3.4").is_none());
    }

    #[test]
    fn reads_rustc_verbose_version() {
        let (version, host) =
            rustc_vv("rustc 1.99.0 (b9 2026-09-28)\nhost: aarch64-apple-darwin\nrelease: 1.99.0\n");
        assert_eq!(version, Some(v("1.99.0")));
        assert_eq!(host.as_deref(), Some("aarch64-apple-darwin"));
        assert_eq!(rustc_vv("rustc 1.98.2 (x)").0, Some(v("1.98.2")));
        assert_eq!(rustc_vv("weird output").0, None);
    }

    #[test]
    fn version_requirement() {
        let req = Requirement {
            version: v("1.99"),
            source: "test".into(),
        };
        assert_eq!(
            msrv_check(Some(v("1.99.0")), Some(&req), true).status,
            CheckStatus::Pass
        );
        let old = msrv_check(Some(v("1.98.5")), Some(&req), true);
        assert_eq!(old.status, CheckStatus::Fail);
        assert_eq!(old.hint.as_deref(), Some("rustup update stable"));
        let unmanaged = msrv_check(Some(v("1.98.5")), Some(&req), false);
        assert_eq!(
            unmanaged.hint.as_deref(),
            Some("install Rust 1.99.0 or newer")
        );
        assert_eq!(msrv_check(None, Some(&req), true).status, CheckStatus::Skip);
        assert_eq!(
            msrv_check(Some(v("1.0")), None, true).status,
            CheckStatus::Skip
        );
    }

    #[test]
    fn linux_hints_follow_os_release() {
        assert_eq!(
            linux_linker_hint("ID=ubuntu\n"),
            "sudo apt install build-essential"
        );
        assert_eq!(
            linux_linker_hint("ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\n"),
            "sudo dnf install gcc"
        );
        assert_eq!(linux_linker_hint("ID=alpine"), "sudo apk add build-base");
        assert!(linux_linker_hint("").contains("package manager"));
    }

    /// A directory of executable stub programs for a fake `PATH`.
    #[cfg(unix)]
    pub(crate) fn stub_bin(tag: &str, programs: &[(&str, &str)]) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = env::temp_dir().join(format!("siderite-stub-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, script) in programs {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    #[cfg(unix)]
    #[test]
    fn missing_tools_are_failures_not_errors() {
        let empty = stub_bin("empty", &[]);
        let probe = Probe::with_path(&empty, &empty);
        let (checks, facts) = toolchain_checks(&probe, None);
        let status = |id: &str| checks.iter().find(|c| c.id == id).unwrap().status;
        assert_eq!(status("toolchain.rustc"), CheckStatus::Fail);
        assert_eq!(status("toolchain.cargo"), CheckStatus::Fail);
        assert_eq!(status("toolchain.rustup"), CheckStatus::Skip);
        assert!(facts.rustc.is_none());
        let rustc = checks.iter().find(|c| c.id == "toolchain.rustc").unwrap();
        assert_eq!(rustc.hint.as_deref(), Some(rustup_install_hint()));
    }

    #[cfg(unix)]
    #[test]
    fn stub_toolchain_is_compared_with_requirement() {
        let bin = stub_bin(
            "old",
            &[
                (
                    "rustc",
                    "printf 'rustc 1.98.0 (x)\\nhost: test-host\\nrelease: 1.98.0\\n'",
                ),
                ("cargo", "echo 'cargo 1.98.0'"),
                ("rustup", "exit 0"),
                ("cc", "echo 'stub cc 1.0'"),
                ("xcode-select", "echo /stub"),
            ],
        );
        let probe = Probe::with_path(&bin, &bin);
        let req = Requirement {
            version: v("1.99"),
            source: "test".into(),
        };
        let (checks, facts) = toolchain_checks(&probe, Some(&req));
        assert_eq!(facts.host.as_deref(), Some("test-host"));
        assert!(facts.rustup);
        let ids: Vec<(&str, CheckStatus)> =
            checks.iter().map(|c| (c.id.as_str(), c.status)).collect();
        assert_eq!(
            ids,
            [
                ("toolchain.rustc", CheckStatus::Pass),
                ("toolchain.cargo", CheckStatus::Pass),
                ("toolchain.rustup", CheckStatus::Pass),
                ("toolchain.version", CheckStatus::Fail),
                ("toolchain.linker", CheckStatus::Pass),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn uninstalled_pinned_toolchain_is_not_reported_missing() {
        let bin = stub_bin(
            "pinned",
            &[
                (
                    "rustc",
                    "echo \"error: toolchain '1.80.0-x' is not installed\" >&2; exit 1",
                ),
                (
                    "cargo",
                    "echo \"error: toolchain '1.80.0-x' is not installed\" >&2; exit 1",
                ),
                ("rustup", "exit 0"),
            ],
        );
        let (checks, _) = toolchain_checks(&Probe::with_path(&bin, &bin), None);
        for id in ["toolchain.rustc", "toolchain.cargo"] {
            let check = checks.iter().find(|c| c.id == id).unwrap();
            assert_eq!(check.status, CheckStatus::Fail);
            assert!(check.message.contains("is not installed"), "{check:?}");
            assert_eq!(check.hint.as_deref(), Some("rustup toolchain install"));
        }
        let rustup = checks.iter().find(|c| c.id == "toolchain.rustup").unwrap();
        assert_eq!(rustup.status, CheckStatus::Pass);
    }

    #[cfg(unix)]
    #[test]
    fn probes_disable_rustup_auto_install() {
        let bin = stub_bin(
            "auto",
            &[("rustc", "echo \"release: 1.99.0 $RUSTUP_AUTO_INSTALL\"")],
        );
        let probe = Probe::with_path(&bin, &bin);
        assert_eq!(
            probe.output("rustc", &[]).as_deref(),
            Some("release: 1.99.0 0")
        );
    }
}
