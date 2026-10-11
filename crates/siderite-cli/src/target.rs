//! Choose the application package and binary to delegate to.
//!
//! Rules, in order:
//!
//! 1. `--package` / `--manifest-path` / `--bin` always win.
//! 2. Otherwise the nearest package whose directory contains the cwd.
//! 3. Otherwise (a virtual workspace root, or a directory that is not in
//!    a package) the one workspace package that depends on `siderite` and
//!    has a binary. Several candidates is a usage error naming each.
//!
//! In the chosen package an app command runs `default-run`, else the only
//! binary; several binaries without either are a usage error.

use crate::error::CliError;
use crate::workspace::{self, Package, Workspace};
use std::path::{Path, PathBuf};

/// Flags that take a value and are accepted before the command.
const VALUE_FLAGS: &[&str] = &[
    "--database-url",
    "--database",
    "--addr",
    "--migrations-dir",
    "--path",
];

/// Project selection flags given before the command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProjectArgs {
    /// `--manifest-path PATH`.
    pub manifest_path: Option<PathBuf>,
    /// `--package NAME` / `-p NAME`.
    pub package: Option<String>,
    /// `--bin NAME`.
    pub bin: Option<String>,
}

impl ProjectArgs {
    fn is_explicit(&self) -> bool {
        self.manifest_path.is_some() || self.package.is_some()
    }
}

/// Project flags and the arguments left for the command.
pub(crate) type Split = Result<(ProjectArgs, Vec<String>), CliError>;

/// Take the project flags that precede the command out of `args`.
///
/// Scanning stops at the first positional argument or `--`; everything
/// from there on (including the app's own `-p` or `--bin`) is returned
/// unchanged.
pub(crate) fn split_project(args: &[String]) -> Split {
    let mut found = ProjectArgs::default();
    let mut rest = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if arg == "--" || !arg.starts_with('-') {
            rest.extend_from_slice(&args[i - 1..]);
            break;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with('-') => (f, Some(v)),
            _ => (arg.as_str(), None),
        };
        let slot = match flag {
            "--manifest-path" | "--package" | "-p" | "--bin" => flag,
            _ => {
                rest.push(arg.clone());
                let takes = VALUE_FLAGS.contains(&flag) && inline.is_none();
                if takes && let Some(value) = args.get(i) {
                    rest.push(value.clone());
                    i += 1;
                }
                continue;
            }
        };
        let value = match inline {
            Some(v) => v.to_owned(),
            None => {
                let v = args.get(i).ok_or_else(|| needs_value(slot))?;
                i += 1;
                v.clone()
            }
        };
        if value.is_empty() {
            return Err(CliError::usage(format!("{slot} requires a value")));
        }
        store(&mut found, slot, value)?;
    }
    Ok((found, rest))
}

fn needs_value(flag: &str) -> CliError {
    CliError::usage(format!("{flag} requires a value"))
}

type Stored = Result<(), CliError>;

fn store(found: &mut ProjectArgs, flag: &str, value: String) -> Stored {
    let twice = || CliError::usage(format!("{flag} given more than once"));
    match flag {
        "--manifest-path" if found.manifest_path.is_none() => {
            found.manifest_path = Some(PathBuf::from(value));
        }
        "--bin" if found.bin.is_none() => found.bin = Some(value),
        "--package" | "-p" if found.package.is_none() => {
            found.package = Some(value);
        }
        _ => return Err(twice()),
    }
    Ok(())
}

/// The application package and binary chosen for a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selected {
    /// Package name.
    pub package: String,
    /// Directory to run cargo in.
    pub dir: PathBuf,
    /// `--bin` to pass to cargo, only when it must be named.
    pub bin: Option<String>,
}

/// Find the app for `cwd`; `Ok(None)` when there is none and nothing was
/// asked for explicitly (the caller then reports its usual message).
///
/// `one_bin` is true for commands that run a binary (`run`, `routes`...)
/// and false for `build` / `test`, which cover every target.
///
/// Without project flags, a plain single-binary package is found by
/// reading manifests, without running `cargo metadata`.
pub(crate) fn locate(
    cargo: &str,
    cwd: &Path,
    args: &ProjectArgs,
    one_bin: bool,
) -> Result<Option<Selected>, CliError> {
    let explicit = args.is_explicit() || args.bin.is_some();
    let mut lenient = false;
    if !explicit {
        match quick_scan(cwd) {
            Quick::Simple(dir) => {
                let package = package_name(&dir);
                return Ok(Some(Selected {
                    package,
                    dir,
                    bin: None,
                }));
            }
            Quick::NoPackage => lenient = true,
            Quick::Complex => {}
        }
    }
    let manifest = match &args.manifest_path {
        Some(path) => Some(resolve_manifest(cwd, path)?),
        None => None,
    };
    if manifest.is_none() && !has_manifest_above(cwd) {
        return explicit_or_none(args, "no Cargo.toml found");
    }
    let ws = match workspace::load(cargo, cwd, manifest.as_deref()) {
        Ok(ws) => ws,
        // Nothing the old lookup could use: stay in standalone mode.
        Err(_) if lenient => return Ok(None),
        Err(err) => return Err(err),
    };
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.into());
    let manifest = manifest.map(|m| canon(&m));
    let found = select(&ws, args, manifest.as_deref(), &canon(cwd))?;
    let Some(pkg) = found else {
        return explicit_or_none(args, "no siderite application found");
    };
    let bin = resolve_bin(pkg, args.bin.as_deref(), one_bin)?;
    Ok(Some(Selected {
        package: pkg.name.clone(),
        dir: pkg.dir().to_path_buf(),
        bin,
    }))
}

/// Outcome of reading manifests above the cwd.
enum Quick {
    /// A package with no extra binaries: run cargo in this directory.
    Simple(PathBuf),
    /// A package that needs `cargo metadata` to choose a binary.
    Complex,
    /// No `[package]` manifest above the cwd.
    NoPackage,
}

/// Walk up to the nearest manifest with a `[package]` table.
fn quick_scan(cwd: &Path) -> Quick {
    for dir in cwd.ancestors() {
        let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
            continue;
        };
        if !text.contains("[package]") {
            continue;
        }
        if is_simple(dir, &text) {
            return Quick::Simple(dir.to_path_buf());
        }
        return Quick::Complex;
    }
    Quick::NoPackage
}

/// Cheap textual check that a package has at most one binary and no
/// `default-run`. Anything unusual (any array-of-tables, a `bin` key,
/// `autobins`, `src/bin/`) is "not simple" so metadata decides.
fn is_simple(dir: &Path, text: &str) -> bool {
    if dir.join("src/bin").exists() {
        return false;
    }
    let mut has_package = false;
    for line in text.lines().map(str::trim) {
        has_package |= line.starts_with("[package]");
        let odd = line.starts_with("[[")
            || line.starts_with("bin")
            || line.contains("autobins")
            || line.contains("default-run")
            || line.contains("default_run");
        if odd {
            return false;
        }
    }
    has_package
}

/// Best-effort `name = "..."` of the package table (display only).
fn package_name(dir: &Path) -> String {
    let text = std::fs::read_to_string(dir.join("Cargo.toml"));
    let text = text.unwrap_or_default();
    let line = text.lines().map(str::trim).find(|l| l.starts_with("name"));
    let value = line.and_then(|l| l.split_once('=')).map(|(_, v)| v);
    let name = value.map(|v| v.trim().trim_matches('"'));
    name.map(str::to_owned).unwrap_or_else(|| {
        dir.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into())
    })
}

type Located = Result<Option<Selected>, CliError>;

fn explicit_or_none(args: &ProjectArgs, why: &str) -> Located {
    if args.is_explicit() || args.bin.is_some() {
        return Err(CliError::usage(format!(
            "cannot select an application: {why}"
        )));
    }
    Ok(None)
}

/// The manifest to use; a directory means its `Cargo.toml`.
fn resolve_manifest(cwd: &Path, given: &Path) -> Result<PathBuf, CliError> {
    let mut path = cwd.join(given);
    if path.is_dir() {
        path.push("Cargo.toml");
    }
    if !path.is_file() {
        return Err(CliError::usage(format!(
            "--manifest-path {}: no such file",
            given.display()
        )));
    }
    Ok(path)
}

/// Whether `cwd` or an ancestor holds a `Cargo.toml`.
fn has_manifest_above(cwd: &Path) -> bool {
    cwd.ancestors().any(|dir| dir.join("Cargo.toml").is_file())
}

/// Pick the package per the module rules; `None` when implicit
/// selection finds no application. `manifest` and `cwd` are canonical,
/// for comparison with the (canonicalized) package directories.
pub(crate) fn select<'a>(
    ws: &'a Workspace,
    args: &ProjectArgs,
    manifest: Option<&Path>,
    cwd: &Path,
) -> Result<Option<&'a Package>, CliError> {
    if let Some(name) = &args.package {
        return named(ws, name).map(Some);
    }
    if let Some(manifest) = manifest
        && let Some(pkg) = ws.packages.iter().find(|p| same(p, manifest))
    {
        return Ok(Some(pkg));
    }
    if manifest.is_none() {
        let nearest = ws
            .packages
            .iter()
            .filter(|p| cwd.starts_with(p.canonical_dir()))
            .max_by_key(|p| p.dir().components().count());
        if nearest.is_some() {
            return Ok(nearest);
        }
    }
    let mut apps = ws
        .packages
        .iter()
        .filter(|p| p.uses_siderite && !p.bins.is_empty());
    let (first, second) = (apps.next(), apps.next());
    match (first, second) {
        (Some(only), None) => Ok(Some(only)),
        (None, _) => Ok(None),
        (Some(a), Some(b)) => {
            let mut list = vec![a, b];
            list.extend(apps);
            Err(ambiguous(&list))
        }
    }
}

fn same(pkg: &Package, manifest: &Path) -> bool {
    let file = pkg.manifest_path.canonicalize();
    file.unwrap_or_else(|_| pkg.manifest_path.clone()) == manifest
}

fn named<'a>(ws: &'a Workspace, name: &str) -> Result<&'a Package, CliError> {
    ws.packages.iter().find(|p| p.name == name).ok_or_else(|| {
        let all = ws.packages.iter();
        let names: Vec<&str> = all.map(|p| p.name.as_str()).collect();
        CliError::usage(format!(
            "no package `{name}` in this workspace (members: {})",
            names.join(", ")
        ))
    })
}

fn ambiguous(apps: &[&Package]) -> CliError {
    let mut text = String::from("several siderite applications:\n");
    for pkg in apps {
        text.push_str(&format!("  {} ({})\n", pkg.name, pkg.dir().display()));
    }
    text.push_str(
        "choose one with --package NAME (or --manifest-path PATH), \
         or cd into its directory",
    );
    CliError::usage(text)
}

/// Binary to run in `pkg`: explicit, `default-run`, or the only one.
///
/// `None` means cargo's own choice is right (no `--bin` needed).
pub(crate) fn resolve_bin(
    pkg: &Package,
    explicit: Option<&str>,
    one_bin: bool,
) -> Result<Option<String>, CliError> {
    let list = pkg.bins.join(", ");
    if let Some(bin) = explicit {
        if pkg.bins.iter().any(|b| b == bin) {
            return Ok(Some(bin.to_owned()));
        }
        return Err(CliError::usage(format!(
            "package `{}` has no binary `{bin}` (binaries: {})",
            pkg.name,
            if list.is_empty() { "none" } else { &list }
        )));
    }
    if !one_bin || pkg.default_run.is_some() {
        return Ok(None);
    }
    match pkg.bins.len() {
        1 => Ok(None),
        0 => Err(CliError::usage(format!(
            "package `{}` has no binary target to run",
            pkg.name
        ))),
        _ => Err(CliError::usage(format!(
            "package `{}` has several binaries ({list}); choose one with \
             --bin NAME or set `default-run` in its Cargo.toml",
            pkg.name
        ))),
    }
}

#[cfg(test)]
mod tests;
