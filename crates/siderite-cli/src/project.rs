//! Cargo project discovery and argument forwarding.

use crate::args::GlobalArgs;
use crate::error::CliError;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Commands that `AppCli` in the application binary understands.
pub const APP_COMMANDS: &[&str] = &[
    "run",
    "routes",
    "check",
    "dbshell",
    "makemigrations",
    "migrate",
    "rollback",
    "showmigrations",
    "inspectmigrations",
    "squashmigrations",
];

/// Cargo subcommands `siderite` forwards verbatim in the app package.
pub const CARGO_COMMANDS: &[&str] = &["build", "test"];

/// Whether `command` is forwarded to the application binary.
#[must_use]
pub fn is_app_command(command: &str) -> bool {
    APP_COMMANDS.contains(&command)
}

/// Resolved target application project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProject {
    /// Directory to run cargo commands from.
    pub package_dir: PathBuf,
    /// Explicit manifest path, if known.
    pub manifest_path: Option<PathBuf>,
    /// Package name to target, if in a workspace.
    pub package_name: Option<String>,
    /// Binary target name, if multiple exist.
    pub binary_name: Option<String>,
}

#[derive(Deserialize)]
struct MetadataOutput {
    packages: Vec<PackageInfo>,
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
struct PackageInfo {
    name: String,
    id: String,
    manifest_path: String,
    targets: Vec<TargetInfo>,
}

#[derive(Deserialize)]
struct TargetInfo {
    name: String,
    kind: Vec<String>,
}

/// Find a Cargo package or workspace and resolve project target.
///
/// # Errors
/// Usage error when project is missing, ambiguous, or flags invalid.
pub fn resolve_project(
    cwd: &Path,
    global: &GlobalArgs,
    command: &str,
) -> Result<ResolvedProject, CliError> {
    if let Some(manifest) = &global.manifest_path {
        if !manifest.exists() {
            return Err(CliError::usage(format!(
                "manifest path `{}` does not exist",
                manifest.display()
            )));
        }
        if !manifest.is_file() {
            return Err(CliError::usage(format!(
                "manifest path `{}` is not a file",
                manifest.display()
            )));
        }
        let package_dir = manifest
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        return resolve_with_metadata(&package_dir, Some(manifest), global, command);
    }

    let manifest_root = find_cargo_manifest(cwd).ok_or_else(|| {
        CliError::usage(format!(
            "`{command}` needs an application. cd into a project created \
             with `siderite new`, or an example directory"
        ))
    })?;

    resolve_with_metadata(&manifest_root, None, global, command)
}

fn resolve_with_metadata(
    start_dir: &Path,
    explicit_manifest: Option<&Path>,
    global: &GlobalArgs,
    command: &str,
) -> Result<ResolvedProject, CliError> {
    let mut cmd = Command::new("cargo");
    cmd.args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(start_dir);
    if let Some(manifest) = explicit_manifest {
        cmd.arg("--manifest-path").arg(manifest);
    }
    let output = cmd.output().map_err(|err| {
        CliError::Io(format!(
            "cannot run cargo metadata (is cargo on PATH?): {err}"
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::usage(format!(
            "cargo metadata failed: {}",
            stderr.trim()
        )));
    }

    let meta: MetadataOutput = serde_json::from_slice(&output.stdout)
        .map_err(|err| CliError::Io(format!("failed to parse cargo metadata: {err}")))?;

    let members: Vec<&PackageInfo> = meta
        .packages
        .iter()
        .filter(|p| meta.workspace_members.contains(&p.id))
        .collect();

    let selected_pkg = if let Some(pkg_name) = &global.package {
        members
            .iter()
            .find(|p| &p.name == pkg_name)
            .copied()
            .ok_or_else(|| {
                let available: Vec<&str> = members.iter().map(|p| p.name.as_str()).collect();
                CliError::usage(format!(
                    "package `{pkg_name}` not found in workspace \
                     (available: {})",
                    available.join(", ")
                ))
            })?
    } else if let Some(pkg) = members.iter().find(|p| {
        let p_dir = Path::new(&p.manifest_path).parent();
        p_dir == Some(start_dir)
    }) {
        pkg
    } else if members.len() == 1 {
        members[0]
    } else {
        let names: Vec<&str> = members.iter().map(|p| p.name.as_str()).collect();
        return Err(CliError::usage(format!(
            "workspace has multiple packages ({}); specify one with -p \
             or --package <NAME>",
            names.join(", ")
        )));
    };

    let manifest_path = PathBuf::from(&selected_pkg.manifest_path);
    let package_dir = manifest_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let binary_name = resolve_binary(selected_pkg, global, command)?;

    Ok(ResolvedProject {
        package_dir,
        manifest_path: Some(manifest_path),
        package_name: Some(selected_pkg.name.clone()),
        binary_name,
    })
}

fn resolve_binary(
    pkg: &PackageInfo,
    global: &GlobalArgs,
    command: &str,
) -> Result<Option<String>, CliError> {
    let bin_targets: Vec<&TargetInfo> = pkg
        .targets
        .iter()
        .filter(|t| t.kind.iter().any(|k| k == "bin"))
        .collect();

    if let Some(bin) = &global.bin {
        if !bin_targets.iter().any(|t| &t.name == bin) {
            let available: Vec<&str> = bin_targets.iter().map(|t| t.name.as_str()).collect();
            return Err(CliError::usage(format!(
                "binary `{bin}` not found in package `{}` (available: {})",
                pkg.name,
                available.join(", ")
            )));
        }
        return Ok(Some(bin.clone()));
    }

    if command == "run" && bin_targets.len() > 1 {
        let names: Vec<&str> = bin_targets.iter().map(|t| t.name.as_str()).collect();
        return Err(CliError::usage(format!(
            "package `{}` has multiple binaries ({}); specify one with \
             --bin <NAME>",
            pkg.name,
            names.join(", ")
        )));
    }

    if bin_targets.len() == 1 {
        return Ok(Some(bin_targets[0].name.clone()));
    }

    Ok(None)
}

/// Walk up from `start` to a directory containing a `Cargo.toml`.
#[must_use]
pub fn find_cargo_manifest(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Walk up from `start` to a `Cargo.toml` that defines a `[package]`.
#[must_use]
pub fn find_app_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file()
            && let Ok(text) = std::fs::read_to_string(&manifest)
            && text.contains("[package]")
        {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// `cargo run -- args` for `project`. Forwards the child's exit code.
///
/// # Errors
/// Return IO error when cargo fails to launch.
pub fn cargo_run(project: &ResolvedProject, args: &[String]) -> Result<u8, CliError> {
    let mut command = Command::new("cargo");
    command.arg("run").current_dir(&project.package_dir);
    if let Some(manifest) = &project.manifest_path {
        command.arg("--manifest-path").arg(manifest);
    }
    if let Some(pkg) = &project.package_name {
        command.arg("--package").arg(pkg);
    }
    if let Some(bin) = &project.binary_name {
        command.arg("--bin").arg(bin);
    }
    command.arg("--").args(args);
    let status = command
        .status()
        .map_err(|err| CliError::Io(format!("cannot run cargo run (is cargo on PATH?): {err}")))?;
    Ok(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1))
}

/// `cargo <cargo_cmd>` with forwarded arguments for `project`.
///
/// # Errors
/// Return IO error when cargo fails to launch.
pub fn cargo_passthrough(
    project: &ResolvedProject,
    cargo_cmd: &str,
    args: &[String],
) -> Result<u8, CliError> {
    let mut command = Command::new("cargo");
    command.arg(cargo_cmd).current_dir(&project.package_dir);
    let forwarded = args_after(cargo_cmd, args);
    let has_pkg = forwarded
        .iter()
        .any(|a| a == "-p" || a.starts_with("--package"));
    if !has_pkg && let Some(pkg) = &project.package_name {
        command.arg("--package").arg(pkg);
    }
    let has_manifest = forwarded.iter().any(|a| a.starts_with("--manifest-path"));
    if !has_manifest && let Some(manifest) = &project.manifest_path {
        command.arg("--manifest-path").arg(manifest);
    }
    command.args(&forwarded);
    let status = command.status().map_err(|err| {
        CliError::Io(format!(
            "cannot run cargo {cargo_cmd} (is cargo on PATH?): {err}"
        ))
    })?;
    Ok(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1))
}

fn args_after(cargo_cmd: &str, args: &[String]) -> Vec<String> {
    args.iter()
        .skip_while(|a| a.as_str() != cargo_cmd)
        .skip(1)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_package_and_skips_a_workspace_root() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(find_app_dir(&root).as_deref(), Some(root.as_path()));
        let workspace = root.parent().unwrap().parent().unwrap();
        let found = find_app_dir(workspace);
        assert!(
            found.as_deref() != Some(workspace),
            "workspace root has no [package]: {found:?}"
        );
    }

    #[test]
    fn passthrough_forwards_args_after_the_command() {
        let args: Vec<String> = ["build", "--release", "--features", "x"]
            .iter()
            .map(|a| (*a).to_owned())
            .collect();
        assert_eq!(args_after("build", &args), args[1..].to_vec());
        assert!(args_after("test", &args[..1]).is_empty());
    }

    #[test]
    fn resolves_project_with_explicit_package() {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let global = GlobalArgs {
            package: Some("siderite-cli".into()),
            ..GlobalArgs::default()
        };
        let resolved = resolve_project(&workspace, &global, "run").unwrap();
        assert_eq!(resolved.package_name.as_deref(), Some("siderite-cli"));
        assert_eq!(resolved.binary_name.as_deref(), Some("siderite"));
    }

    #[test]
    fn multiple_packages_without_selection_returns_usage_error() {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let global = GlobalArgs::default();
        let err = resolve_project(&workspace, &global, "run").unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("multiple packages"));
    }
}
