//! `siderite setup`: guided check of the prerequisites for building a
//! Siderite application.
//!
//! Setup only reports. It never reads stdin, never runs an installer or
//! `rustup`, and never edits a shell profile or the global toolchain; the
//! next steps it prints are commands for the developer to run. Output does
//! not depend on whether a terminal is attached.

use crate::args::GlobalArgs;
use crate::doctor::report_json;
use crate::error::CliError;
use crate::toolchain::{
    Check, CheckStatus, Probe, Requirement, RustVersion, render_checks, toolchain_checks,
};
use serde::Serialize;
use std::path::Path;

/// Minimum Rust version of the installed framework.
pub const FRAMEWORK_RUST_VERSION: &str = env!("CARGO_PKG_RUST_VERSION");

/// Result of `siderite setup`.
#[derive(Debug, Clone, Serialize)]
pub struct SetupReport {
    /// Operating system and architecture this CLI was built for.
    pub platform: Platform,
    /// Minimum Rust version required by this framework version.
    pub required_rust: String,
    /// Toolchain checks in a fixed order.
    pub checks: Vec<Check>,
    /// Commands to run, in order. Empty when everything is ready.
    pub next_steps: Vec<String>,
    /// `true` when no check failed.
    pub ready: bool,
}

/// Platform description.
#[derive(Debug, Clone, Serialize)]
pub struct Platform {
    /// `std::env::consts::OS`.
    pub os: String,
    /// `std::env::consts::ARCH`.
    pub arch: String,
    /// Host triple reported by `rustc`, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rust_host: Option<String>,
}

/// Run the setup checks with `probe`.
#[must_use]
pub fn setup_report(probe: &Probe) -> SetupReport {
    let requirement = RustVersion::parse(FRAMEWORK_RUST_VERSION).map(|version| Requirement {
        version,
        source: format!("siderite {}", env!("CARGO_PKG_VERSION")),
    });
    let (checks, facts) = toolchain_checks(probe, requirement.as_ref());
    let ready = checks.iter().all(|c| c.status != CheckStatus::Fail);
    let next_steps = next_steps(&checks);
    SetupReport {
        platform: Platform {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            rust_host: facts.host,
        },
        required_rust: requirement.map_or_else(
            || FRAMEWORK_RUST_VERSION.to_owned(),
            |r| r.version.to_string(),
        ),
        checks,
        next_steps,
        ready,
    }
}

/// Hints of failed checks first, then of warnings, without duplicates.
fn next_steps(checks: &[Check]) -> Vec<String> {
    let mut steps: Vec<String> = Vec::new();
    for status in [CheckStatus::Fail, CheckStatus::Warn] {
        for hint in checks
            .iter()
            .filter(|c| c.status == status)
            .filter_map(|c| c.hint.clone())
        {
            if !steps.contains(&hint) {
                steps.push(hint);
            }
        }
    }
    if checks.iter().any(|c| c.status == CheckStatus::Fail) {
        steps.push("siderite setup".to_owned());
    }
    steps
}

/// Text form of `report`.
#[must_use]
pub fn render_setup(report: &SetupReport) -> String {
    let mut out =
        String::from("Siderite setup: prerequisite check (nothing is installed or changed)\n");
    out.push_str(&format!(
        "Platform: {} {}{}\n",
        report.platform.os,
        report.platform.arch,
        report
            .platform
            .rust_host
            .as_deref()
            .map(|h| format!(" (rustc host {h})"))
            .unwrap_or_default()
    ));
    out.push_str(&format!(
        "Requires: Rust {} or newer\n\n",
        report.required_rust
    ));
    out.push_str(&render_checks(&report.checks));
    if report.next_steps.is_empty() {
        out.push_str("\nReady. Create an application with: siderite new NAME\n");
    } else {
        out.push_str(if report.ready {
            "\nReady, with suggestions:\n"
        } else {
            "\nNot ready. Next steps:\n"
        });
        for (i, step) in report.next_steps.iter().enumerate() {
            out.push_str(&format!("  {}. {step}\n", i + 1));
        }
    }
    out
}

/// `siderite setup [--json]`. Exit code 1 when a prerequisite is missing.
///
/// # Errors
/// Only when JSON serialization fails.
pub fn run(cwd: &Path, global: &GlobalArgs) -> Result<u8, CliError> {
    let report = setup_report(&Probe::system(cwd));
    if global.json {
        println!("{}", report_json("setup", report.ready, &report)?);
    } else {
        print!("{}", render_setup(&report));
    }
    Ok(if report.ready { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn framework_version_parses() {
        assert!(RustVersion::parse(FRAMEWORK_RUST_VERSION).is_some());
    }

    #[test]
    fn next_steps_put_failures_first_and_dedupe() {
        let checks = vec![
            Check::new("a", CheckStatus::Warn, "w").hint("warn step"),
            Check::new("b", CheckStatus::Fail, "f").hint("install"),
            Check::new("c", CheckStatus::Fail, "f").hint("install"),
            Check::new("d", CheckStatus::Pass, "p"),
        ];
        assert_eq!(
            next_steps(&checks),
            ["install", "warn step", "siderite setup"]
        );
        assert!(next_steps(&[Check::new("d", CheckStatus::Pass, "p")]).is_empty());
    }

    #[cfg(unix)]
    mod with_stubs {
        use super::*;
        use crate::toolchain::rustup_install_hint;
        use crate::toolchain::tests::stub_bin;

        const CC: (&str, &str) = ("cc", "echo 'stub cc'");
        const XCODE: (&str, &str) = ("xcode-select", "echo /stub");

        fn rustc(version: &str) -> String {
            format!("printf 'rustc {version} (x)\\nhost: stub-host\\nrelease: {version}\\n'")
        }

        #[test]
        fn empty_path_lists_install_steps() {
            let bin = stub_bin("setup-empty", &[]);
            let report = setup_report(&Probe::with_path(&bin, &bin));
            assert!(!report.ready);
            assert_eq!(report.next_steps[0], rustup_install_hint());
            assert_eq!(report.next_steps.last().unwrap(), "siderite setup");
            assert!(render_setup(&report).contains("Not ready. Next steps:\n  1. "));
        }

        #[test]
        fn old_rustc_with_rustup_suggests_update() {
            let old = rustc("1.0.0");
            let bin = stub_bin(
                "setup-old",
                &[
                    ("rustc", &old),
                    ("cargo", "echo 'cargo 1.0.0'"),
                    ("rustup", "exit 0"),
                    CC,
                    XCODE,
                ],
            );
            let report = setup_report(&Probe::with_path(&bin, &bin));
            assert!(!report.ready);
            assert_eq!(
                report.next_steps,
                ["rustup update stable", "siderite setup"]
            );
            assert_eq!(report.platform.rust_host.as_deref(), Some("stub-host"));
        }

        #[test]
        fn distro_rust_without_rustup_is_ready_with_a_note() {
            let current = rustc("9.0.0");
            let bin = stub_bin(
                "setup-distro",
                &[
                    ("rustc", &current),
                    ("cargo", "echo 'cargo 9.0.0'"),
                    CC,
                    XCODE,
                ],
            );
            let report = setup_report(&Probe::with_path(&bin, &bin));
            assert!(report.ready);
            let rustup = report
                .checks
                .iter()
                .find(|c| c.id == "toolchain.rustup")
                .unwrap();
            assert_eq!(rustup.status, CheckStatus::Warn);
            assert!(render_setup(&report).contains("Ready, with suggestions:"));
        }

        #[test]
        fn json_and_text_agree() {
            let current = rustc("9.0.0");
            let bin = stub_bin(
                "setup-json",
                &[
                    ("rustc", &current),
                    ("cargo", "echo 'cargo 9.0.0'"),
                    ("rustup", "exit 0"),
                    CC,
                    XCODE,
                ],
            );
            let report = setup_report(&Probe::with_path(&bin, &bin));
            let json: serde_json::Value =
                serde_json::from_str(&report_json("setup", report.ready, &report).unwrap())
                    .unwrap();
            assert_eq!(json["ok"], serde_json::Value::Bool(report.ready));
            assert_eq!(json["data"]["required_rust"], report.required_rust.as_str());
            let text = render_setup(&report);
            for check in json["data"]["checks"].as_array().unwrap() {
                let line = format!(
                    "[{}] {}",
                    check["status"].as_str().unwrap(),
                    check["id"].as_str().unwrap()
                );
                assert!(text.contains(&line), "{line}");
            }
            if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
                assert!(report.ready, "{text}");
                assert!(text.contains("Ready. Create an application"));
            }
        }
    }
}
