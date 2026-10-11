//! `siderite setup [--json] [--install]`: are the build prerequisites
//! in place, and what exactly to run when they are not.
//!
//! It checks rustup, `rustc` against siderite's minimum Rust version,
//! `cargo`, `rustfmt`, `clippy` and a C linker, and prints the next step
//! for this operating system. It never prompts, so a terminal and a
//! script get the same report. Nothing is installed unless `--install` is
//! given, and even then only through `rustup` (toolchain and components);
//! shell profiles and system packages are never touched.

use super::checks::{Tools, toolchain};
use super::tools::{version_output, which};
use super::{Findings, Report, print};
use crate::error::CliError;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Rustup's installer instructions for this platform.
const RUSTUP: &str = if cfg!(windows) {
    "download and run rustup-init.exe from https://rustup.rs"
} else {
    "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
};

/// How to get a C linker on this platform.
const LINKER: &str = if cfg!(target_os = "macos") {
    "xcode-select --install"
} else if cfg!(windows) {
    "install the Visual Studio Build Tools (C++ workload)"
} else {
    "sudo apt install build-essential (Debian, Ubuntu) or \
     sudo dnf install gcc (Fedora)"
};

/// Parsed `setup` arguments.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Options {
    /// `--json`.
    pub json: bool,
    /// `--install`: run the rustup steps.
    pub install: bool,
}

/// Parse the arguments after the project flags.
///
/// Errors:
///   A usage error for anything else.
pub(crate) fn parse(raw: &[String]) -> Result<Options, CliError> {
    let mut options = Options::default();
    for arg in raw {
        match arg.as_str() {
            "setup" => {}
            "--json" => options.json = true,
            "--install" => options.install = true,
            other => {
                let msg = format!(
                    "unexpected argument `{other}`; usage: setup [--json] \
                     [--install]"
                );
                return Err(CliError::usage(msg));
            }
        }
    }
    if options.json && options.install {
        let msg = "--install prints progress; run it without --json";
        return Err(CliError::usage(msg));
    }
    Ok(options)
}

/// The prerequisite report for `path`, probing tools in `dir`.
pub(crate) fn diagnose(path: &OsStr, dir: &Path) -> Report {
    findings(path, dir).into_report("setup")
}

fn findings(path: &OsStr, dir: &Path) -> Findings {
    let mut f = Findings::default();
    rustup(path, dir, &mut f);
    let cargo = which("cargo", path);
    let tools = Tools {
        dir,
        path,
        cargo: cargo.as_deref(),
    };
    toolchain(&tools, None, &mut f);
    linker(path, &mut f);
    f
}

fn rustup(path: &OsStr, dir: &Path, f: &mut Findings) {
    let name = "rustup";
    let found = which(name, path);
    let probe = |p: PathBuf| version_output(&p, &["--version"], dir, path);
    let version = found.and_then(probe);
    match version {
        Some(version) => f.ok(name, &version),
        None => {
            let detail = "rustup not found: the toolchain cannot be updated \
                          or extended by `setup --install`";
            f.info(name, detail);
        }
    }
}

/// A C linker: rustc links through the platform's C toolchain.
fn linker(path: &OsStr, f: &mut Findings) {
    let name = "linker";
    if cfg!(windows) {
        let detail = "rustc finds the MSVC linker through Visual Studio; \
                      if builds fail with `link.exe` not found, install \
                      the Build Tools";
        f.info(name, detail);
        return;
    }
    let found = ["cc", "gcc", "clang"].iter().find_map(|c| which(c, path));
    match found {
        Some(found) => f.ok(name, &found.display().to_string()),
        None => {
            let detail = "no C linker (cc, gcc or clang) on PATH";
            f.error(name, "setup.E001", (detail, LINKER));
        }
    }
}

#[cfg(test)]
pub(crate) fn steps_for(path: &OsStr, dir: &Path) -> Vec<String> {
    let found = findings(path, dir);
    steps(&found).iter().map(|s| s.join(" ")).collect()
}

/// `siderite setup` from the process environment.
///
/// Errors:
///   A usage error, or an IO error writing the report.
pub(crate) fn run(raw: &[String], cwd: &Path) -> Result<u8, CliError> {
    let options = parse(raw)?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    if options.install {
        install(&path, cwd);
    }
    let report = diagnose(&path, cwd);
    print(&report, options.json)?;
    Ok(report.exit_code())
}

/// Run the rustup steps the report calls for, showing each command.
fn install(path: &OsStr, dir: &Path) {
    let before = findings(path, dir);
    let Some(rustup) = which("rustup", path) else {
        let how = format!("install it with:\n  {RUSTUP}");
        eprintln!("setup: rustup is not installed; {how}");
        return;
    };
    for step in steps(&before) {
        eprintln!("setup: running rustup {}", step.join(" "));
        let mut command = Command::new(&rustup);
        let status = command.args(step).current_dir(dir).status();
        if !status.is_ok_and(|s| s.success()) {
            eprintln!("setup: rustup {} failed", step.join(" "));
        }
    }
    if before.has("setup.E001") {
        let how = format!("install one with:\n  {LINKER}");
        eprintln!("setup: rustup cannot install a C linker; {how}");
    }
}

/// The rustup commands that fix what `findings` reports.
fn steps(findings: &Findings) -> Vec<&'static [&'static str]> {
    let mut steps: Vec<&'static [&'static str]> = Vec::new();
    if findings.has("doctor.E001") || findings.has("doctor.E003") {
        steps.push(&["toolchain", "install", "stable"]);
    }
    if findings.has("doctor.E001") {
        // No toolchain at all yet: make the new one the default.
        steps.push(&["default", "stable"]);
    }
    if findings.has("doctor.W001") {
        steps.push(&["component", "add", "rustfmt"]);
    }
    if findings.has("doctor.W002") {
        steps.push(&["component", "add", "clippy"]);
    }
    steps
}
