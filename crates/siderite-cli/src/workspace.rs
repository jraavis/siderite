//! Workspace discovery through `cargo metadata --no-deps`.

use crate::error::CliError;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The crate whose dependents are siderite applications.
const FRAMEWORK: &str = "siderite";

/// One workspace member, as far as project selection cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Package {
    /// Package name.
    pub name: String,
    /// Absolute path of its `Cargo.toml`.
    pub manifest_path: PathBuf,
    /// Whether it depends on `siderite`.
    pub uses_siderite: bool,
    /// Binary target names, in manifest order.
    pub bins: Vec<String>,
    /// `default-run` of the package, when set.
    pub default_run: Option<String>,
    /// Names of the normal (not dev or build) dependencies.
    pub dependencies: Vec<String>,
    /// `rust-version` of the package (resolved from the workspace).
    pub rust_version: Option<String>,
    /// The package version.
    pub version: String,
    /// `[package.metadata.siderite]`, or `null`.
    pub siderite: Value,
}

impl Package {
    /// Directory holding the manifest.
    pub fn dir(&self) -> &Path {
        self.manifest_path.parent().unwrap_or(Path::new(""))
    }

    /// Canonical directory, for comparisons only (on Windows this is a
    /// `\\?\` verbatim path that must not be shown or run in).
    pub fn canonical_dir(&self) -> PathBuf {
        let dir = self.dir();
        dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
    }
}

/// The members of a Cargo workspace (or the one package of a crate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Workspace {
    /// Members sorted by name, then manifest path.
    pub packages: Vec<Package>,
    /// The workspace root directory, when reported.
    pub root: Option<PathBuf>,
    /// The target directory, when reported.
    pub target_dir: Option<PathBuf>,
}

/// Message for a `cargo` that cannot be started.
pub(crate) fn spawn_error(what: &str, err: &std::io::Error) -> CliError {
    if err.kind() == std::io::ErrorKind::NotFound {
        return CliError::Io(format!(
            "cannot run {what}: `cargo` was not found on PATH \
             (install Rust from https://rustup.rs, or add \
             ~/.cargo/bin to PATH)"
        ));
    }
    CliError::Io(format!("cannot run {what}: {err}"))
}

/// Run `cargo metadata` from `cwd` (for `manifest`, when given).
///
/// Arguments are passed as separate argv entries, never through a shell.
pub(crate) fn load(
    cargo: &str,
    cwd: &Path,
    manifest: Option<&Path>,
) -> Result<Workspace, CliError> {
    let mut command = Command::new(cargo);
    command
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(cwd);
    if let Some(manifest) = manifest {
        command.arg("--manifest-path").arg(manifest);
    }
    let output = command
        .output()
        .map_err(|err| spawn_error("cargo metadata", &err))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::Io(format!(
            "cargo metadata failed: {}",
            stderr.trim()
        )));
    }
    parse(&String::from_utf8_lossy(&output.stdout))
}

/// Parse `cargo metadata --format-version 1 --no-deps` output.
pub(crate) fn parse(json: &str) -> Result<Workspace, CliError> {
    let bad = |w: &str| CliError::Io(format!("bad cargo metadata output: {w}"));
    let parsed = serde_json::from_str(json);
    let root: Value = parsed.map_err(|err| bad(&err.to_string()))?;
    let listed = root
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("no `packages` array"))?;
    let mut packages = Vec::with_capacity(listed.len());
    for item in listed {
        packages.push(package(item).ok_or_else(|| bad("bad package"))?);
    }
    packages.sort_by_key(|p| (p.name.clone(), p.manifest_path.clone()));
    let field = |key: &str| root.get(key).and_then(Value::as_str);
    let dir = |key: &str| field(key).map(PathBuf::from);
    Ok(Workspace {
        packages,
        root: dir("workspace_root"),
        target_dir: dir("target_directory"),
    })
}

fn package(item: &Value) -> Option<Package> {
    let name = item.get("name")?.as_str()?.to_owned();
    let manifest = PathBuf::from(item.get("manifest_path")?.as_str()?);
    let manifest_path = manifest;
    let listed = item.get("dependencies").and_then(Value::as_array);
    let uses_siderite = listed.is_some_and(|d| d.iter().any(is_framework_dep));
    let dependencies = listed
        .into_iter()
        .flatten()
        .filter(|d| is_normal(d))
        .filter_map(|d| d.get("name")?.as_str().map(str::to_owned))
        .collect();
    let bins = item
        .get("targets")
        .and_then(Value::as_array)
        .map(|targets| targets.iter().filter_map(bin_name).collect())
        .unwrap_or_default();
    let default_run = item
        .get("default_run")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let rust_version = item
        .get("rust_version")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let version = item.get("version").and_then(Value::as_str);
    let version = version.unwrap_or_default().to_owned();
    let metadata = item.get("metadata").and_then(|m| m.get(FRAMEWORK));
    Some(Package {
        name,
        manifest_path,
        uses_siderite,
        dependencies,
        bins,
        default_run,
        rust_version,
        version,
        siderite: metadata.cloned().unwrap_or(Value::Null),
    })
}

/// A normal (not dev or build) dependency.
fn is_normal(dep: &Value) -> bool {
    dep.get("kind").is_none_or(Value::is_null)
}

/// A normal dependency on the framework crate.
fn is_framework_dep(dep: &Value) -> bool {
    let name = dep.get("name").and_then(Value::as_str);
    is_normal(dep) && name == Some(FRAMEWORK)
}

fn bin_name(target: &Value) -> Option<String> {
    let is_bin = target
        .get("kind")?
        .as_array()?
        .iter()
        .any(|k| k.as_str() == Some("bin"));
    if !is_bin {
        return None;
    }
    Some(target.get("name")?.as_str()?.to_owned())
}
