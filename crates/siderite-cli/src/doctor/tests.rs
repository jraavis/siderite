//! Doctor checks against stand-in toolchains and scratch projects.

#![allow(clippy::unwrap_used)]

use super::checks::{FRAMEWORK_MSRV, address_check, rustc_check};
use super::*;
use crate::fixtures::{ScratchDir, block_on};
use serde_json::Value;
use std::path::Path;

const SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/schemas/cli-report-v1.json"
));

fn status_of(report: &Envelope<DoctorData>, name: &str) -> Status {
    let check = report.data.checks.iter().find(|c| c.name == name);
    check.unwrap_or_else(|| panic!("no check {name}")).status
}

fn codes(report: &Envelope<DoctorData>) -> Vec<&str> {
    report.diagnostics.iter().map(Diagnostic::code).collect()
}

fn probe(path: OsString, cwd: &Path) -> Probe {
    Probe {
        path,
        cwd: cwd.to_path_buf(),
        project: ProjectArgs::default(),
        process_env: false,
    }
}

#[cfg(unix)]
fn fake_toolchain(dir: &Path, rustc: &str, clippy: bool) -> OsString {
    use std::os::unix::fs::PermissionsExt;
    let script = |name: &str, body: &str| {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mode = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&path, mode).unwrap();
    };
    script("rustc", &format!("echo '{rustc}'"));
    let clippy = if clippy {
        "echo 'clippy 0.1.99'"
    } else {
        "exit 1"
    };
    script(
        "cargo",
        &format!(
            "case \"$1\" in\n\
             --version) echo 'cargo 1.99.0 (x 2026-09-01)';;\n\
             fmt) echo 'rustfmt 1.8.0';;\n\
             clippy) {clippy};;\n\
             *) exit 2;;\n\
             esac"
        ),
    );
    dir.as_os_str().to_owned()
}

#[cfg(unix)]
#[test]
fn a_healthy_toolchain_passes_with_advisory_clients() {
    let tools = ScratchDir::new("doctor-tools");
    let cwd = ScratchDir::new("doctor-cwd");
    let rustc = format!("rustc {FRAMEWORK_MSRV}.0 (abc 2026-09-28)");
    let path = fake_toolchain(tools.path(), &rustc, true);
    let report = block_on(diagnose(&probe(path, cwd.path()), None));
    assert!(report.ok, "{}", render(&report));
    assert_eq!(status_of(&report, "toolchain.rustc"), Status::Ok);
    assert_eq!(status_of(&report, "toolchain.cargo"), Status::Ok);
    assert_eq!(status_of(&report, "toolchain.clippy"), Status::Ok);
    assert_eq!(status_of(&report, "project"), Status::Info);
    assert_eq!(status_of(&report, "config"), Status::Ok);
    // Clients missing from PATH are information, never failures.
    assert_eq!(status_of(&report, "client.psql"), Status::Info);
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let value = serde_json::to_value(&report).unwrap();
    let errors: Vec<_> = validator.iter_errors(&value).collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[cfg(unix)]
#[test]
fn old_rustc_and_missing_components_are_flagged() {
    let tools = ScratchDir::new("doctor-old");
    let cwd = ScratchDir::new("doctor-old-cwd");
    let path = fake_toolchain(tools.path(), "rustc 1.80.0 (old)", false);
    let report = block_on(diagnose(&probe(path, cwd.path()), None));
    assert!(!report.ok);
    assert_eq!(report.exit_code(), 1);
    assert_eq!(codes(&report), ["doctor.E003", "doctor.W002"]);
    let text = render(&report);
    assert!(text.contains("siderite needs Rust"), "{text}");
    assert!(text.contains("rustup component add clippy"), "{text}");
    assert!(text.ends_with("1 error(s) and 1 warning(s).\n"), "{text}");
}

#[test]
fn missing_tools_are_actionable_errors() {
    let cwd = ScratchDir::new("doctor-none");
    let empty = ScratchDir::new("doctor-empty-path");
    let path = empty.path().as_os_str().to_owned();
    let report = block_on(diagnose(&probe(path, cwd.path()), None));
    let found = codes(&report);
    for code in ["doctor.E001", "doctor.E002", "doctor.W001", "doctor.W002"] {
        assert!(found.contains(&code), "{found:?}");
    }
    let hint = report.diagnostics[0].hint().unwrap();
    assert!(hint.contains("rustup"), "{hint}");
}

#[test]
fn project_msrv_is_compared() {
    let mut f = Findings::default();
    rustc_check(Some("rustc 1.99.0 (x)"), Some("1.200"), &mut f);
    let report = f.into_envelope();
    assert_eq!(codes(&report), ["doctor.E004"]);
    let mut f = Findings::default();
    rustc_check(Some("rustc 9.0.0 (x)"), Some("1.200"), &mut f);
    let detail = &f.checks[0].detail;
    assert!(detail.contains("project 1.200"), "{detail}");
    let mut f = Findings::default();
    rustc_check(Some("weird"), None, &mut f);
    assert_eq!(f.into_envelope().diagnostics[0].code(), "doctor.W006");
}

fn config_report(toml: &str) -> Envelope<DoctorData> {
    let cwd = ScratchDir::new("doctor-config");
    std::fs::write(cwd.path().join("siderite.toml"), toml).unwrap();
    let mut f = Findings::default();
    let p = probe(OsString::new(), cwd.path());
    if let Some(settings) = checks::config(&p, cwd.path(), &mut f) {
        checks::databases(&settings, &mut f);
    }
    f.into_envelope()
}

#[test]
fn malformed_config_is_an_error_without_secrets() {
    let report = config_report("[server\naddr = ");
    assert_eq!(codes(&report), ["doctor.E005"]);
    let report = config_report("[databases.default]\nurl = 12345\n");
    assert_eq!(codes(&report), ["doctor.E005"]);
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("12345"), "{json}");
}

#[test]
fn database_schemes_and_compiled_backends() {
    let toml = "[databases.default]\nurl = \"ftp://u:hunter2@h/db\"\n\
                [databases.cache]\nurl = \"redis://h\"\n\
                [databases.main]\nurl = \"postgres://u:hunter3@h/db\"\n\
                [databases.local]\nurl = \"sqlite::memory:\"\n";
    let report = config_report(toml);
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("hunter"), "{json}");
    assert_eq!(status_of(&report, "database.default"), Status::Error);
    assert_eq!(status_of(&report, "database.cache"), Status::Info);
    assert_eq!(status_of(&report, "database.local"), Status::Ok);
    let pg = status_of(&report, "database.main");
    let want = if cfg!(feature = "postgres") {
        Status::Ok
    } else {
        Status::Warning
    };
    assert_eq!(pg, want);
}

#[test]
fn a_busy_address_is_an_advisory_warning() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let mut f = Findings::default();
    address_check(&addr, &mut f);
    let report = f.into_envelope();
    assert!(report.ok, "a warning never fails the report");
    assert_eq!(codes(&report), ["doctor.W004"]);
    drop(listener);
    let mut f = Findings::default();
    address_check(&addr, &mut f);
    assert!(f.checks[0].detail.contains("may change"));
}
