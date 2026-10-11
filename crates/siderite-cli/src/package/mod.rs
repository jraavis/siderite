//! `siderite package --release [--output DIR] [--force] [--json]`: a
//! directory holding the app's release binary for the host target, its
//! declared static assets and its migration files, with a manifest of
//! versions and SHA-256 checksums.
//!
//! The build is `cargo build --release --locked`: the dependency
//! versions are exactly those of `Cargo.lock`, which must exist and be
//! current. Assets are declared in the package manifest:
//!
//! ```toml
//! [package.metadata.siderite.package]
//! assets = ["static", "templates"]
//! migrations = "migrations"   # the default, when the directory exists
//! ```
//!
//! Credentials, databases, build caches and VCS data never ship: `.env*`,
//! `*.db`, `*.sqlite*`, keys, `.git*`, `target` and `.siderite` are left
//! out wherever they appear, and a selected text file that holds a
//! credential stops packaging. The packaged binary is then run once
//! (`routes`) with an empty `PATH`, so it is known to work without Cargo.
//! Packaging never deploys and never applies migrations.

use crate::dev::build::{BuildSpec, Built, build};
use crate::error::CliError;
use crate::fsio::{io_error, unique_name};
use crate::report::text::diagnostic_line;
use crate::report::{Diagnostic, Envelope, channel};
use crate::target::{self, ProjectArgs};
use crate::workspace::{self, Package};
use serde::Serialize;
use serde_json::Value;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod files;
mod native;
#[cfg(test)]
mod tests;

use files::{Entry, Selection};
use native::Native;

const USAGE: &str = "usage: package --release [--output DIR] \
                     [--force] [--json]";
const NEEDS_APP: &str = "package needs an application package; cd into one";
/// The manifest file in the package.
const MANIFEST: &str = "manifest.json";
/// Checksums in `sha256sum -c` format.
const SUMS: &str = "SHA256SUMS";
/// No lock file, or the release build failed.
const NOT_BUILT: &str = "package.E001";
/// A selected file holds a credential.
const CREDENTIAL: &str = "package.E002";
/// Version of the manifest layout.
const FORMAT: u32 = 1;

type Result<T> = std::result::Result<T, CliError>;

/// Parsed arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    /// `--output DIR`, absolute.
    pub output: Option<PathBuf>,
    /// `--force`: replace an earlier package at the output.
    pub force: bool,
    /// `--json`.
    pub json: bool,
}

/// Parse the arguments after the project flags (`package ...`).
///
/// Errors:
///   A usage error without `--release`, or for an unknown flag.
pub(crate) fn parse(raw: &[String], cwd: &Path) -> Result<Options> {
    let mut args = raw.iter().skip_while(|a| *a != "package").skip(1);
    let mut options = Options {
        output: None,
        force: false,
        json: false,
    };
    let mut release = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--release" => release = true,
            "--force" => options.force = true,
            "--json" => options.json = true,
            "--output" => {
                let missing = || CliError::usage(USAGE);
                let dir = args.next().ok_or_else(missing)?;
                options.output = Some(cwd.join(dir));
            }
            other => {
                let msg = format!("unexpected argument `{other}`; {USAGE}");
                return Err(CliError::usage(msg));
            }
        }
    }
    if !release {
        let msg = format!("only release packages are built; {USAGE}");
        return Err(CliError::usage(msg));
    }
    Ok(options)
}

/// The paths `[package.metadata.siderite.package]` declares, plus
/// `migrations` when it exists and nothing else is declared for it.
///
/// Errors:
///   A usage error for a malformed declaration.
pub(crate) fn declared(meta: &Value, dir: &Path) -> Result<Vec<String>> {
    let section = meta.get("package").unwrap_or(&Value::Null);
    let bad = |what: &str| {
        let msg = format!(
            "[package.metadata.siderite.package] {what} must be {}",
            if what == "assets" {
                "a list of paths"
            } else {
                "a path"
            }
        );
        CliError::usage(msg)
    };
    let mut paths = Vec::new();
    if let Some(assets) = section.get("assets") {
        let list = assets.as_array().ok_or_else(|| bad("assets"))?;
        for item in list {
            let path = item.as_str().ok_or_else(|| bad("assets"))?;
            paths.push(path.to_owned());
        }
    }
    match section.get("migrations") {
        Some(Value::String(path)) => paths.push(path.clone()),
        Some(_) => return Err(bad("migrations")),
        None if dir.join("migrations").is_dir() => {
            paths.push("migrations".to_owned());
        }
        None => {}
    }
    Ok(paths)
}

/// The manifest written into the package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Manifest {
    /// Layout version.
    pub format: u32,
    /// Package name.
    pub package: String,
    /// Package version.
    pub version: String,
    /// The binary, inside the package.
    pub binary: String,
    /// The target triple it was built for (the host).
    pub target: String,
    /// Always `release`.
    pub profile: &'static str,
    /// Built with `--locked` from `Cargo.lock`.
    pub locked: bool,
    /// `rustc --version`.
    pub rustc: String,
    /// The siderite versions in `Cargo.lock`.
    pub siderite: Vec<String>,
    /// Every file but the manifest and checksums, sorted.
    pub files: Vec<Entry>,
    /// Declared paths left out by the exclusion rules.
    pub excluded: Vec<String>,
    /// Shared libraries the binary needs at run time.
    pub native: Native,
}

/// The `data` of a `package` envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PackageData {
    /// The package directory.
    pub output: String,
    /// Its manifest.
    pub manifest: Manifest,
}

/// The selected application package's metadata.
fn package(app_dir: &Path) -> Result<(Package, Option<PathBuf>)> {
    let ws = workspace::load("cargo", app_dir, None)?;
    let dir = app_dir.canonicalize().unwrap_or_else(|_| app_dir.into());
    let found = ws.packages.into_iter().find(|p| p.canonical_dir() == dir);
    let missing = || CliError::usage(NEEDS_APP);
    Ok((found.ok_or_else(missing)?, ws.target_dir))
}

/// `rustc --version` and the host triple, from `rustc -vV` in `dir`.
fn toolchain(dir: &Path) -> Result<(String, String)> {
    let output = Command::new("rustc").arg("-vV").current_dir(dir).output();
    let output = output.map_err(|e| workspace::spawn_error("rustc", &e))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text.lines().next().unwrap_or_default().to_owned();
    let host = text.lines().find_map(|l| l.strip_prefix("host: "));
    let host = host.ok_or_else(|| CliError::Io("rustc -vV: no host".into()))?;
    Ok((version, host.to_owned()))
}

/// Make room for the package at `out`: only an earlier package (a
/// directory with a manifest) is ever removed, and only with `force`.
///
/// Errors:
///   A conflict for anything at `out` that may not be replaced.
pub(crate) fn make_room(out: &Path, force: bool) -> Result<()> {
    if !out.exists() {
        return Ok(());
    }
    let earlier = out.join(MANIFEST).is_file();
    if !(force && earlier) {
        let shown = out.display();
        let why = if earlier {
            "pass --force to replace it"
        } else {
            "it is not a siderite package; choose another --output"
        };
        return Err(CliError::Conflict(format!("{shown} exists; {why}")));
    }
    std::fs::remove_dir_all(out).map_err(|e| io_error("remove", out, &e))
}

/// Run `binary routes` from `dir` with an empty `PATH` (no Cargo).
///
/// Errors:
///   `package.E003` when it fails.
fn smoke(binary: &Path, dir: &Path) -> std::result::Result<(), Diagnostic> {
    let empty = dir.join(".no-path");
    let output = Command::new(binary)
        .arg("routes")
        .current_dir(dir)
        .env("PATH", &empty)
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let last = stderr.lines().last().unwrap_or_default();
            Err(error("package.E003", &format!("`routes` failed: {last}")))
        }
        Err(err) => Err(error("package.E003", &format!("cannot run: {err}"))),
    }
}

fn error(code: &str, message: &str) -> Diagnostic {
    let diag = Diagnostic::new(code, crate::report::Severity::Error, message);
    let hint = "the packaged binary must run without Cargo; see \
                `siderite explain` for the code";
    diag.with_hint(hint)
}

type Report = Result<Envelope<PackageData>>;
type Exit = Result<u8>;

/// Write the package for `app` into a staging directory beside `out`,
/// then move it into place.
async fn assemble(app: &target::Selected, options: &Options) -> Report {
    let (pkg, target_dir) = package(&app.dir)?;
    let paths = declared(&pkg.siderite, &app.dir)?;
    let selection = files::select(&app.dir, &paths)?;
    if let Some(file) = files::credentials(&selection.files) {
        return Err(CliError::usage(format!(
            "{CREDENTIAL}: `{file}` looks like it holds a credential; \
             remove it or leave it out of the declared assets"
        )));
    }
    let locked = app.dir.ancestors().any(|d| d.join("Cargo.lock").is_file());
    if !locked {
        return Err(CliError::usage(format!(
            "{NOT_BUILT}: no Cargo.lock; run `siderite build` once to \
             create it, then review and commit it"
        )));
    }
    let (rustc, host) = toolchain(&app.dir)?;
    let spec = BuildSpec {
        cargo: PathBuf::from("cargo"),
        dir: app.dir.clone(),
        bin: app.bin.clone().or_else(|| app.default_run.clone()),
        release: true,
    };
    let Built::Ready(binary) = build(&spec).await? else {
        return Err(CliError::Io(format!(
            "{NOT_BUILT}: the release build failed (diagnostics above; \
             a stale Cargo.lock fails with --locked)"
        )));
    };
    let name = format!("{}-{}-{host}", pkg.name, pkg.version);
    let root = target_dir.unwrap_or_else(|| app.dir.join("target"));
    let default = root.join("siderite-package").join(name);
    let out = options.output.clone().unwrap_or(default);
    make_room(&out, options.force)?;
    let parent = out.parent().unwrap_or(Path::new("."));
    let made = std::fs::create_dir_all(parent);
    made.map_err(|e| io_error("create", parent, &e))?;
    let staging = parent.join(unique_name(".siderite-package", ".tmp"));
    let written = write(&staging, &binary, &selection);
    let (files, bin) = match written {
        Ok(done) => done,
        Err(err) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
    };
    let manifest = Manifest {
        format: FORMAT,
        package: pkg.name.clone(),
        version: pkg.version.clone(),
        native: native::libraries(&staging.join(&bin)),
        binary: bin.clone(),
        target: host,
        profile: "release",
        locked: true,
        rustc,
        siderite: crate::docs::version::installed(&app.dir),
        files,
        excluded: selection.excluded,
    };
    finish(&staging, &manifest)?;
    let smoked = smoke(&staging.join(&bin), &staging);
    let diags = match smoked {
        Ok(()) => {
            let moved = std::fs::rename(&staging, &out);
            moved.map_err(|e| io_error("move into place", &out, &e))?;
            Vec::new()
        }
        Err(diag) => {
            let _ = std::fs::remove_dir_all(&staging);
            vec![diag]
        }
    };
    let data = PackageData {
        output: out.display().to_string(),
        manifest,
    };
    Ok(Envelope::new("package", data, diags))
}

type Written = Result<(Vec<Entry>, String)>;

/// Copy the binary and the selected files into `staging`.
fn write(staging: &Path, binary: &Path, selection: &Selection) -> Written {
    let file = binary.file_name().unwrap_or_default().to_string_lossy();
    let bin = format!("bin/{file}");
    let mut entries = vec![files::copy(binary, &staging.join(&bin), &bin)?];
    for source in &selection.files {
        let to = staging.join(&source.rel);
        entries.push(files::copy(&source.path, &to, &source.rel)?);
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((entries, bin))
}

/// Write the manifest and the checksum list.
fn finish(staging: &Path, manifest: &Manifest) -> Result<()> {
    let json = serde_json::to_string_pretty(manifest);
    let json = json.map_err(|e| CliError::Io(e.to_string()))? + "\n";
    let path = staging.join(MANIFEST);
    std::fs::write(&path, json).map_err(|e| io_error("write", &path, &e))?;
    let mut sums = String::new();
    for entry in &manifest.files {
        let _ = writeln!(sums, "{}  {}", entry.sha256, entry.path);
    }
    let path = staging.join(SUMS);
    std::fs::write(&path, sums).map_err(|e| io_error("write", &path, &e))
}

/// Text form: where the package is and what it holds.
pub(crate) fn render(report: &Envelope<PackageData>) -> String {
    let data = &report.data;
    let m = &data.manifest;
    let mut out = String::new();
    if report.ok {
        let (name, version) = (&m.package, &m.version);
        let _ = writeln!(out, "packaged {name} {version} for {}", m.target);
        let _ = writeln!(out, "  {}", data.output);
    }
    let _ = writeln!(out, "  {} files, binary {}", m.files.len(), m.binary);
    if !m.excluded.is_empty() {
        let _ = writeln!(out, "  left out: {}", m.excluded.join(", "));
    }
    let libs = if m.native.libraries.is_empty() {
        "none reported".to_owned()
    } else {
        m.native.libraries.join(", ")
    };
    let _ = writeln!(out, "  native libraries: {libs}");
    for diag in &report.diagnostics {
        let _ = writeln!(out, "{}", diagnostic_line(diag));
    }
    out
}

/// `siderite package` from `cwd`.
///
/// Errors:
///   Usage errors, a package that cannot be built or written, or an IO
///   error writing the report.
pub(crate) async fn run(raw: &[String], cwd: &Path, p: &ProjectArgs) -> Exit {
    let options = parse(raw, cwd)?;
    let app = target::locate("cargo", cwd, p, true)?;
    let app = app.ok_or_else(|| CliError::usage(NEEDS_APP))?;
    let report = assemble(&app, &options).await?;
    if options.json {
        channel::emit_json(&report, None)?;
    } else {
        print!("{}", render(&report));
    }
    Ok(report.exit_code())
}
