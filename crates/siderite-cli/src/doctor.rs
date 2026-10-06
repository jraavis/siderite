//! `siderite doctor`: offline toolchain, project, configuration and feature
//! checks.
//!
//! Database clients and the listen port are advisory (`warn` at most). No
//! check connects to a database or changes a file; `cargo metadata` runs with
//! `--offline`.

use crate::args::GlobalArgs;
use crate::envelope::{CliEnvelope, ENVELOPE_SCHEMA_VERSION};
use crate::error::CliError;
use crate::project::{self, MetadataOutput, PackageInfo};
use crate::toolchain::{
    Check, CheckStatus, Probe, Requirement, RustVersion, render_checks, toolchain_checks,
};
use serde::Serialize;
use siderite_config::{ConfigBuilder, DEFAULT_CONFIG_FILE, DEFAULT_ENV_PREFIX, Settings};
use std::collections::BTreeSet;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

/// Result of `siderite doctor`.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    /// Checks in a fixed order: toolchain, project, config, features,
    /// clients, network.
    pub checks: Vec<Check>,
}

impl DoctorReport {
    /// `true` when no check failed.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.status != CheckStatus::Fail)
    }
}

/// The selected package, as far as it could be resolved.
struct ProjectFacts {
    dir: PathBuf,
    name: String,
    rust_version: Option<String>,
    /// Features enabled on direct `siderite*` dependencies.
    siderite_features: BTreeSet<String>,
}

/// Run every doctor check.
#[must_use]
pub fn doctor_report(cwd: &Path, global: &GlobalArgs, probe: &Probe) -> DoctorReport {
    let (project_check, project) = project_check(cwd, global, probe);
    let declared = project.as_ref().and_then(|p| {
        let version = RustVersion::parse(p.rust_version.as_deref()?)?;
        Some(Requirement {
            version,
            source: format!("package `{}` rust-version", p.name),
        })
    });
    // Without a declared rust-version, the framework's minimum still applies.
    let requirement = declared.clone().or_else(|| {
        Some(Requirement {
            version: RustVersion::parse(crate::setup::FRAMEWORK_RUST_VERSION)?,
            source: format!("siderite {}", env!("CARGO_PKG_VERSION")),
        })
    });
    let toolchain_probe = project
        .as_ref()
        .map_or_else(|| probe.clone(), |p| probe.in_dir(&p.dir));
    let (mut checks, _) = toolchain_checks(&toolchain_probe, requirement.as_ref());
    checks.push(project_check);
    if let Some(project) = &project {
        if let Some(raw) = &project.rust_version
            && declared.is_none()
        {
            checks.push(Check::new(
                "project.rust_version",
                CheckStatus::Warn,
                format!("rust-version `{raw}` could not be parsed"),
            ));
        }
        project_checks(project, global, probe, &mut checks);
    }
    DoctorReport { checks }
}

fn project_check(cwd: &Path, global: &GlobalArgs, probe: &Probe) -> (Check, Option<ProjectFacts>) {
    let id = "project";
    let manifest = match &global.manifest_path {
        Some(path) if !path.is_file() => {
            return (
                Check::new(
                    id,
                    CheckStatus::Fail,
                    format!("manifest path `{}` is not a file", path.display()),
                ),
                None,
            );
        }
        Some(path) => path.clone(),
        None => match project::find_cargo_manifest(cwd) {
            Some(dir) => dir.join("Cargo.toml"),
            None => {
                return (
                    Check::new(
                        id,
                        CheckStatus::Skip,
                        "no Cargo.toml found; project checks skipped",
                    ),
                    None,
                );
            }
        },
    };
    let start_dir = manifest.parent().unwrap_or(cwd).to_path_buf();
    let output = probe
        .in_dir(&start_dir)
        .cargo()
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
        ])
        .arg("--manifest-path")
        .arg(&manifest)
        .output();
    let output = match output {
        Err(_) => {
            return (
                Check::new(
                    id,
                    CheckStatus::Skip,
                    "cargo unavailable; project not inspected",
                ),
                None,
            );
        }
        Ok(out) if !out.status.success() => {
            let first = crate::toolchain::first_line(&String::from_utf8_lossy(&out.stderr));
            let hint = crate::toolchain::failure_hint(&first)
                .unwrap_or("fix Cargo.toml so `cargo metadata` succeeds");
            return (
                Check::new(
                    id,
                    CheckStatus::Fail,
                    format!("cargo metadata failed: {first}"),
                )
                .hint(hint),
                None,
            );
        }
        Ok(out) => out,
    };
    let meta: MetadataOutput = match serde_json::from_slice(&output.stdout) {
        Ok(meta) => meta,
        Err(err) => {
            return (
                Check::new(
                    id,
                    CheckStatus::Fail,
                    format!("unreadable cargo metadata: {err}"),
                ),
                None,
            );
        }
    };
    match project::select_package(&meta, &start_dir, global.package.as_deref()) {
        Ok(pkg) => {
            let facts = facts_of(pkg);
            (
                Check::new(
                    id,
                    CheckStatus::Pass,
                    format!("package `{}` at {}", facts.name, facts.dir.display()),
                ),
                Some(facts),
            )
        }
        Err(err) => (Check::new(id, CheckStatus::Fail, err.to_string()), None),
    }
}

fn facts_of(pkg: &PackageInfo) -> ProjectFacts {
    let manifest = PathBuf::from(&pkg.manifest_path);
    ProjectFacts {
        dir: manifest.parent().map(Path::to_path_buf).unwrap_or_default(),
        name: pkg.name.clone(),
        rust_version: pkg.rust_version.clone(),
        siderite_features: pkg
            .dependencies
            .iter()
            .filter(|d| d.name.starts_with("siderite"))
            .flat_map(|d| d.features.iter().cloned())
            .collect(),
    }
}

/// The same chain as `siderite_config::load()`, rooted at the package.
fn load_settings(dir: &Path) -> Result<Settings, siderite_config::ConfigError> {
    ConfigBuilder::new()
        .file_optional(dir.join(DEFAULT_CONFIG_FILE))
        .env_prefix(DEFAULT_ENV_PREFIX)
        .build()
}

/// Backend family of a database URL: (scheme label, cargo feature, client).
fn backend_of(url: &str) -> Option<(&'static str, Option<&'static str>, &'static str)> {
    let scheme = url::Url::parse(url).ok()?.scheme().to_owned();
    Some(match scheme.as_str() {
        "sqlite" => ("sqlite", None, "sqlite3"),
        "postgres" | "postgresql" => ("postgres", Some("postgres"), "psql"),
        "mysql" | "mariadb" => ("mysql", Some("mysql"), "mysql"),
        "redis" | "rediss" => ("redis", None, ""),
        "mongodb" | "mongodb+srv" => ("mongodb", None, ""),
        _ => return None,
    })
}

fn project_checks(
    project: &ProjectFacts,
    global: &GlobalArgs,
    probe: &Probe,
    checks: &mut Vec<Check>,
) {
    let config_file = project.dir.join(DEFAULT_CONFIG_FILE);
    let settings = match load_settings(&project.dir) {
        Ok(settings) => settings,
        Err(err) => {
            checks.push(
                Check::new("config", CheckStatus::Fail, err.to_string())
                    .hint("fix siderite.toml or the SIDERITE_* environment variables"),
            );
            return;
        }
    };
    let source = if config_file.is_file() {
        "siderite.toml and environment"
    } else {
        "defaults and environment (no siderite.toml)"
    };
    checks.push(Check::new(
        "config",
        CheckStatus::Pass,
        format!(
            "loaded from {source}; database aliases: {}",
            settings.databases.len()
        ),
    ));

    let mut clients = BTreeSet::new();
    for (alias, db) in &settings.databases {
        let id = format!("features.{alias}");
        match backend_of(db.url.expose()) {
            None => checks.push(Check::new(
                &id,
                CheckStatus::Warn,
                format!("database `{alias}` URL is not a recognised database URL"),
            )),
            Some((scheme, feature, client)) => {
                checks.push(match feature {
                    Some(feature) if !project.siderite_features.contains(feature) => Check::new(
                        &id,
                        CheckStatus::Warn,
                        format!(
                            "database `{alias}` uses {scheme}, but no direct siderite \
                             dependency enables feature `{feature}` (it may be enabled \
                             transitively)"
                        ),
                    )
                    .hint(format!(
                        "add features = [\"{feature}\"] to the siderite dependency in Cargo.toml"
                    )),
                    _ => Check::new(
                        &id,
                        CheckStatus::Pass,
                        format!("database `{alias}` uses {scheme}"),
                    ),
                });
                if !client.is_empty() {
                    clients.insert(client);
                }
            }
        }
    }
    for client in clients {
        let id = format!("clients.{client}");
        checks.push(if probe.on_path(client) {
            Check::new(&id, CheckStatus::Pass, format!("{client} found"))
        } else {
            Check::new(
                &id,
                CheckStatus::Warn,
                format!("{client} not found; only `siderite dbshell` needs it"),
            )
        });
    }

    let addr = global.addr.as_ref().unwrap_or(&settings.server.addr);
    checks.push(match TcpListener::bind(addr.as_str()) {
        Ok(listener) => {
            drop(listener);
            Check::new(
                "network.addr",
                CheckStatus::Pass,
                format!("{addr} is free now (another process may take it before startup)"),
            )
        }
        Err(err) => Check::new(
            "network.addr",
            CheckStatus::Warn,
            format!("cannot bind {addr} now: {err} (this can change before startup)"),
        )
        .hint("stop the other process or pass --addr / set server.addr"),
    });
}

/// `siderite doctor [--json]`. Exit code 1 when any check fails.
///
/// # Errors
/// Only when JSON serialization fails.
pub fn run(cwd: &Path, global: &GlobalArgs) -> Result<u8, CliError> {
    let report = doctor_report(cwd, global, &Probe::system(cwd));
    let ok = report.ok();
    if global.json {
        println!("{}", report_json("doctor", ok, &report)?);
    } else {
        print!("{}", render_checks(&report.checks));
        if !ok {
            println!("\nSome required checks failed; see the hints above.");
        }
    }
    Ok(if ok { 0 } else { 1 })
}

/// A JSON envelope that keeps the report as `data` even when `ok` is false.
pub(crate) fn report_json<T: Serialize>(
    command: &str,
    ok: bool,
    report: &T,
) -> Result<String, CliError> {
    let env = CliEnvelope {
        schema_version: ENVELOPE_SCHEMA_VERSION.to_owned(),
        command: command.to_owned(),
        ok,
        data: Some(report),
        diagnostics: Vec::new(),
    };
    env.to_json_pretty()
        .map_err(|err| CliError::Io(format!("failed to serialize JSON: {err}")))
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::toolchain::tests::stub_bin;

    fn project(tag: &str, cargo_toml: &str, siderite_toml: Option<&str>) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("siderite-doctor-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), cargo_toml).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        if let Some(text) = siderite_toml {
            std::fs::write(dir.join("siderite.toml"), text).unwrap();
        }
        dir
    }

    const MANIFEST: &str = "[package]\nname = \"shop\"\nversion = \"0.1.0\"\n\
        edition = \"2024\"\nrust-version = \"1.99\"\n\n[dependencies]\n";

    /// Real cargo for metadata, so these use the system probe.
    fn report(dir: &Path) -> DoctorReport {
        doctor_report(dir, &GlobalArgs::default(), &Probe::system(dir))
    }

    fn status<'a>(report: &'a DoctorReport, id: &str) -> Option<&'a Check> {
        report.checks.iter().find(|c| c.id == id)
    }

    #[test]
    fn healthy_project_passes() {
        let dir = project(
            "ok",
            MANIFEST,
            Some(
                "[server]\naddr = \"127.0.0.1:0\"\n[databases.main]\nurl = \"sqlite://shop.db\"\n",
            ),
        );
        let report = report(&dir);
        let project = status(&report, "project").unwrap();
        assert_eq!(project.status, CheckStatus::Pass, "{project:?}");
        let version = status(&report, "toolchain.version").unwrap();
        assert!(
            version.message.contains("package `shop` rust-version"),
            "{version:?}"
        );
        assert_eq!(status(&report, "config").unwrap().status, CheckStatus::Pass);
        assert_eq!(
            status(&report, "features.main").unwrap().status,
            CheckStatus::Pass
        );
        assert!(status(&report, "clients.sqlite3").is_some());
        assert_eq!(
            status(&report, "network.addr").unwrap().status,
            CheckStatus::Pass
        );
    }

    #[test]
    fn missing_backend_feature_is_advisory() {
        let dir = project(
            "feature",
            MANIFEST,
            Some("[databases.main]\nurl = \"postgres://u:hunter2@dbhost/x\"\n"),
        );
        let report = report(&dir);
        let check = status(&report, "features.main").unwrap();
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("postgres"));
        assert!(status(&report, "clients.psql").unwrap().status != CheckStatus::Fail);
    }

    #[test]
    fn database_urls_never_leak() {
        for (tag, toml) in [
            (
                "valid",
                "[databases.main]\nurl = \"postgres://u:hunter2@dbhost/x\"\n",
            ),
            (
                "malformed",
                "[databases.main]\nurl = \"postgres://u:hunter2@dbhost/x\"\nmax_connections = \"many\"\n",
            ),
            (
                "syntax",
                "[databases.main]\nurl = \"postgres://u:hunter2@dbhost/x\n",
            ),
        ] {
            let dir = project(&format!("leak-{tag}"), MANIFEST, Some(toml));
            let report = report(&dir);
            let text = render_checks(&report.checks);
            let json = report_json("doctor", report.ok(), &report).unwrap();
            for out in [&text, &json] {
                assert!(!out.contains("hunter2"), "{tag}: {out}");
                assert!(!out.contains("dbhost"), "{tag}: {out}");
            }
            if tag != "valid" {
                assert_eq!(
                    status(&report, "config").unwrap().status,
                    CheckStatus::Fail,
                    "{tag}"
                );
                assert!(!report.ok());
            }
        }
    }

    #[test]
    fn json_and_text_list_the_same_checks() {
        let dir = project("same", MANIFEST, None);
        let report = report(&dir);
        let json: serde_json::Value =
            serde_json::from_str(&report_json("doctor", report.ok(), &report).unwrap()).unwrap();
        assert_eq!(json["ok"], serde_json::Value::Bool(report.ok()));
        let text = render_checks(&report.checks);
        let checks = json["data"]["checks"].as_array().unwrap();
        assert_eq!(checks.len(), report.checks.len());
        for (line, check) in text.lines().filter(|l| l.starts_with('[')).zip(checks) {
            let expected = format!(
                "[{}] {}",
                check["status"].as_str().unwrap(),
                check["id"].as_str().unwrap()
            );
            assert!(line.starts_with(&expected), "{line} vs {expected}");
        }
    }

    #[test]
    fn outside_a_project_only_toolchain_runs() {
        let dir = std::env::temp_dir().join(format!("siderite-doctor-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // temp_dir has no Cargo.toml above it on CI runners; if one exists
        // the project check is still not a hard error.
        let report = report(&dir);
        assert!(status(&report, "project").is_some());
        assert!(status(&report, "toolchain.rustc").is_some());
    }

    #[test]
    fn missing_cargo_skips_project_checks() {
        let dir = project("nocargo", MANIFEST, None);
        let empty = stub_bin("doctor-empty", &[]);
        let report = doctor_report(
            &dir,
            &GlobalArgs::default(),
            &Probe::with_path(&dir, &empty),
        );
        assert_eq!(
            status(&report, "project").unwrap().status,
            CheckStatus::Skip
        );
        assert_eq!(
            status(&report, "toolchain.cargo").unwrap().status,
            CheckStatus::Fail
        );
        assert!(!report.ok());
        assert!(status(&report, "config").is_none());
    }

    #[test]
    fn framework_minimum_applies_without_rust_version() {
        let manifest = MANIFEST.replace("rust-version = \"1.99\"\n", "");
        let dir = project("nomsrv", &manifest, None);
        let check = report(&dir)
            .checks
            .into_iter()
            .find(|c| c.id == "toolchain.version")
            .unwrap();
        assert!(check.message.contains("(siderite "), "{check:?}");
    }

    #[test]
    fn addr_flag_overrides_config() {
        let dir = project("addr", MANIFEST, Some("[server]\naddr = \"127.0.0.1:1\"\n"));
        let global = GlobalArgs {
            addr: Some("127.0.0.1:0".into()),
            ..GlobalArgs::default()
        };
        let report = doctor_report(&dir, &global, &Probe::system(&dir));
        let check = status(&report, "network.addr").unwrap();
        assert!(check.message.starts_with("127.0.0.1:0 "), "{check:?}");
    }

    #[test]
    fn bad_manifest_is_a_failed_check() {
        let dir = project("badmanifest", "[package\nname = ", None);
        let report = report(&dir);
        assert_eq!(
            status(&report, "project").unwrap().status,
            CheckStatus::Fail
        );
    }
}
