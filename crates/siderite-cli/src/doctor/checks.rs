//! The individual doctor checks.

use super::tools::{Version, version_output, which};
use super::{Findings, Probe};
use crate::connect::{backend_kind, url_scheme};
use crate::target::{self, Selected};
use crate::workspace;
use siderite_config::{ConfigBuilder, DEFAULT_CONFIG_FILE as FILE};
use siderite_config::{DEFAULT_ENV_PREFIX, DEFAULT_SERVER_ADDR, Settings};
use siderite_orm::BackendKind;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Rust version the framework itself needs (`rust-version` of this crate).
pub(crate) const FRAMEWORK_MSRV: &str = env!("CARGO_PKG_RUST_VERSION");

const INSTALL_HINT: &str = "install Rust with rustup \
     (https://rustup.rs) and put ~/.cargo/bin on PATH";

/// Native clients only `dbshell` uses.
const CLIENTS: [(&str, &str); 3] = [
    ("sqlite3", "SQLite"),
    ("psql", "PostgreSQL"),
    ("mysql", "MySQL"),
];

/// What the offline checks found that live probes build on.
pub(crate) struct Found {
    /// The project directory, else the working directory.
    pub dir: PathBuf,
    /// The loaded configuration, unless it failed to load.
    pub settings: Option<Settings>,
}

/// Run every offline check in report order.
pub(crate) fn run_all(probe: &Probe, f: &mut Findings) -> Found {
    let cargo = which("cargo", &probe.path);
    let project = project(probe, cargo.as_deref(), f);
    let dir = match &project {
        Some(p) => p.dir.clone(),
        None => probe.cwd.clone(),
    };
    let msrv = project.and_then(|p| p.rust_version);
    let tools = Tools {
        dir: &dir,
        path: &probe.path,
        cargo: cargo.as_deref(),
    };
    toolchain(&tools, msrv.as_deref(), f);
    let settings = config(probe, &dir, f);
    if let Some(settings) = &settings {
        databases(settings, f);
    }
    clients(probe, f);
    address(probe, settings.as_ref(), f);
    Found { dir, settings }
}

/// The selected application package.
struct Project {
    dir: PathBuf,
    rust_version: Option<String>,
}

fn project(probe: &Probe, cargo: Cargo<'_>, f: Out<'_>) -> Option<Project> {
    let program = match cargo {
        Some(c) => c.display().to_string(),
        None => "cargo".to_owned(),
    };
    let found = target::locate(&program, &probe.cwd, &probe.project, false);
    match found {
        Ok(Some(Selected { package, dir, .. })) => {
            let detail = format!("package `{package}` in {}", dir.display());
            f.ok("project", &detail);
            let rust_version = match cargo {
                Some(_) => rust_version(&program, &dir, &package),
                None => None,
            };
            Some(Project { dir, rust_version })
        }
        Ok(None) => {
            let detail = "no application package here; only standalone \
                          commands (migrate, showmigrations, ...) apply";
            f.info("project", detail);
            None
        }
        Err(err) => {
            let hint = "pass --manifest-path or --package to choose the app";
            f.warn("project", "doctor.W005", (&err.to_string(), hint));
            None
        }
    }
}

/// `rust-version` of `package` through `cargo metadata`.
fn rust_version(cargo: &str, dir: &Path, package: &str) -> Option<String> {
    let manifest = dir.join("Cargo.toml");
    let ws = workspace::load(cargo, dir, Some(&manifest)).ok()?;
    let found = ws.packages.into_iter().find(|p| p.name == package)?;
    found.rust_version
}

/// Where and how the toolchain is probed.
struct Tools<'a> {
    dir: &'a Path,
    path: &'a OsStr,
    cargo: Option<&'a Path>,
}

fn toolchain(tools: &Tools<'_>, msrv: Option<&str>, f: &mut Findings) {
    let rustc = which("rustc", tools.path);
    let cargo = tools.cargo;
    let jobs: [Job<'_>; 4] = [
        (rustc.as_deref(), &["--version"]),
        (cargo, &["--version"]),
        (cargo, &["fmt", "--version"]),
        (cargo, &["clippy", "--version"]),
    ];
    let [rustc, cargo, fmt, clippy] = probe_all(jobs, tools);
    rustc_check(rustc.as_deref(), msrv, f);
    match cargo {
        Some(version) => f.ok("toolchain.cargo", &version),
        None => {
            let detail = "cargo was not found or did not run";
            f.error("toolchain.cargo", "doctor.E002", (detail, INSTALL_HINT));
        }
    }
    component(("toolchain.rustfmt", "doctor.W001", "rustfmt"), fmt, f);
    component(("toolchain.clippy", "doctor.W002", "clippy"), clippy, f);
}

/// A program (if found) and its `--version` arguments.
type Job<'a> = (Option<&'a Path>, &'static [&'static str]);
type Jobs<'a, const N: usize> = [Job<'a>; N];
type Outputs<const N: usize> = [Option<String>; N];
type Cargo<'a> = Option<&'a Path>;
type Out<'a> = &'a mut Findings;

/// Run the independent version probes side by side.
fn probe_all<const N: usize>(jobs: Jobs<'_, N>, t: &Tools<'_>) -> Outputs<N> {
    let (dir, path) = (t.dir, t.path);
    std::thread::scope(|s| {
        let handles = jobs.map(|(program, args)| {
            s.spawn(move || {
                let program = program?;
                version_output(program, args, dir, path)
            })
        });
        handles.map(|h| h.join().ok().flatten())
    })
}

/// Compare the `rustc --version` output with the framework's and the
/// project's minimum Rust versions.
pub(crate) fn rustc_check(out: Option<&str>, msrv: Option<&str>, f: Out<'_>) {
    let name = "toolchain.rustc";
    let Some(output) = out else {
        let detail = "rustc was not found or did not run";
        f.error(name, "doctor.E001", (detail, INSTALL_HINT));
        return;
    };
    let Some(actual) = Version::from_output(output) else {
        let detail = format!("unrecognised version: {output}");
        let hint = "run `rustc --version` and check the toolchain";
        f.warn(name, "doctor.W006", (&detail, hint));
        return;
    };
    let below = |min: &str| Version::parse(min).is_some_and(|v| actual < v);
    if below(FRAMEWORK_MSRV) {
        let msrv = FRAMEWORK_MSRV;
        let detail = format!("rustc {actual}: siderite needs Rust {msrv}");
        let hint = format!(
            "run `rustup update stable` (or `rustup toolchain install \
             {FRAMEWORK_MSRV}`)"
        );
        f.error(name, "doctor.E003", (&detail, &hint));
        return;
    }
    if let Some(minimum) = msrv.filter(|m| below(m)) {
        let detail = format!("rustc {actual}: rust-version needs {minimum}");
        let hint = format!("run `rustup update` or install Rust {minimum}");
        f.error(name, "doctor.E004", (&detail, &hint));
        return;
    }
    let project = msrv.map(|m| format!(", project {m}")).unwrap_or_default();
    let needs = FRAMEWORK_MSRV;
    let detail = format!("rustc {actual} (siderite needs {needs}{project})");
    f.ok(name, &detail);
}

/// `(check name, diagnostic code, rustup component)`.
type Component<'a> = (&'a str, &'a str, &'a str);

fn component(what: Component<'_>, output: Option<String>, f: &mut Findings) {
    let (name, code, tool) = what;
    match output {
        Some(version) => f.ok(name, &version),
        None => {
            let detail = format!("{tool} is not installed for this toolchain");
            let hint = format!("run `rustup component add {tool}`");
            f.warn(name, code, (&detail, &hint));
        }
    }
}

/// Load the configuration the way `siderite_config::load` does, from the
/// project directory.
pub(crate) fn config(p: &Probe, dir: &Path, f: Out<'_>) -> Option<Settings> {
    let file = dir.join(FILE);
    let mut builder = ConfigBuilder::new().file_optional(&file);
    if p.process_env {
        builder = builder.env_prefix(DEFAULT_ENV_PREFIX);
    }
    match builder.build() {
        Ok(settings) => {
            let detail = if file.exists() {
                format!("{} loads", file.display())
            } else {
                format!("no {FILE}; defaults and environment load")
            };
            f.ok("config", &detail);
            Some(settings)
        }
        Err(err) => {
            let shown = file.display();
            let hint = format!("fix {shown} or the SIDERITE_* variables");
            f.error("config", "doctor.E005", (&err.to_string(), &hint));
            None
        }
    }
}

/// Each configured alias: a known scheme, compiled into this binary.
pub(crate) fn databases(settings: &Settings, f: &mut Findings) {
    for (alias, db) in &settings.databases {
        let name = format!("database.{alias}");
        let url = db.url.expose();
        let scheme = url_scheme(url).unwrap_or_default();
        let label = format!("alias `{alias}` ({scheme})");
        match backend_kind(url) {
            None => unsupported(&name, alias, &scheme, f),
            Some(kind) if !compiled(kind) => missing_feature(&name, kind, f),
            Some(kind @ (BackendKind::Redis | BackendKind::MongoDb)) => {
                let detail = format!("{label}: {kind:?}, set up by the app");
                f.info(&name, &detail);
            }
            Some(_) => f.ok(&name, &label),
        }
    }
}

fn unsupported(name: &str, alias: &str, scheme: &str, f: &mut Findings) {
    let detail = if scheme.is_empty() {
        format!("alias `{alias}`: the URL has no scheme")
    } else {
        format!("alias `{alias}`: unsupported scheme `{scheme}`")
    };
    let hint = "use sqlite:, postgres://, mysql://, mongodb:// or redis://";
    f.error(name, "doctor.E006", (&detail, hint));
}

fn missing_feature(name: &str, kind: BackendKind, f: &mut Findings) {
    let detail = format!(
        "this siderite binary has no {kind:?} support, so standalone \
         migrate cannot use this alias"
    );
    let feature = match kind {
        BackendKind::MySql => "mysql",
        _ => "postgres",
    };
    let hint = format!(
        "reinstall with `cargo install siderite-cli --features {feature}`; \
         the app binary is not affected"
    );
    f.warn(name, "doctor.W003", (&detail, &hint));
}

/// Whether the standalone binary can open `kind`.
fn compiled(kind: BackendKind) -> bool {
    let pg = kind == BackendKind::Postgres && !cfg!(feature = "postgres");
    let my = kind == BackendKind::MySql && !cfg!(feature = "mysql");
    !(pg || my)
}

fn clients(probe: &Probe, f: &mut Findings) {
    for (program, engine) in CLIENTS {
        let name = format!("client.{program}");
        match which(program, &probe.path) {
            Some(found) => f.ok(&name, &found.display().to_string()),
            None => {
                let detail = format!("not found ({engine}); dbshell only");
                f.info(&name, &detail);
            }
        }
    }
}

/// Whether the listen address is free right now. Advisory only.
fn address(probe: &Probe, settings: Option<&Settings>, f: &mut Findings) {
    let env = match probe.process_env {
        true => std::env::var("ADDR").ok().filter(|a| !a.is_empty()),
        false => None,
    };
    let configured = settings.map(|s| s.server.addr.clone());
    let fallback = || DEFAULT_SERVER_ADDR.to_owned();
    address_check(&env.or(configured).unwrap_or_else(fallback), f);
}

/// Try to bind `addr` once and release it at once.
pub(crate) fn address_check(addr: &str, f: &mut Findings) {
    let name = "server.addr";
    let hint = "stop the other process, or use `run --addr` / ADDR";
    match std::net::TcpListener::bind(addr) {
        Ok(listener) => {
            drop(listener);
            let detail = format!("{addr} is free now (may change before run)");
            f.ok(name, &detail);
        }
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            let detail = format!("{addr} is in use right now");
            f.warn(name, "doctor.W004", (&detail, hint));
        }
        Err(err) => {
            let detail = format!("cannot bind {addr}: {err}");
            f.warn(name, "doctor.W004", (&detail, hint));
        }
    }
}
