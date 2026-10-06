//! Cargo project discovery and argument forwarding.

use crate::args::GlobalArgs;
use crate::error::CliError;
use crate::toolchain::{Probe, first_line};
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

/// Commands `siderite` forwards to a cargo subcommand in the app package,
/// with every argument after the command passed through verbatim.
pub const CARGO_COMMANDS: &[&str] = &["build", "test", "fmt", "lint", "clean"];

/// The cargo subcommand behind a [`CARGO_COMMANDS`] entry: `lint` runs
/// Clippy; every other command shares its cargo name.
#[must_use]
pub fn cargo_subcommand(command: &str) -> &str {
    match command {
        "lint" => "clippy",
        other => other,
    }
}

/// Rustup component providing `cargo <sub>`, for subcommands that are not
/// built into cargo.
fn component_for(sub: &str) -> Option<&'static str> {
    match sub {
        "fmt" => Some("rustfmt"),
        "clippy" => Some("clippy"),
        _ => None,
    }
}

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
pub(crate) struct MetadataOutput {
    pub(crate) packages: Vec<PackageInfo>,
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct PackageInfo {
    pub(crate) name: String,
    id: String,
    pub(crate) manifest_path: String,
    targets: Vec<TargetInfo>,
    /// `rust-version`, with workspace inheritance resolved by Cargo.
    #[serde(default)]
    pub(crate) rust_version: Option<String>,
    #[serde(default)]
    pub(crate) dependencies: Vec<DependencyInfo>,
}

#[derive(Deserialize)]
pub(crate) struct DependencyInfo {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) features: Vec<String>,
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

    let selected_pkg = select_package(&meta, start_dir, global.package.as_deref())?;

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

/// Pick the workspace member a command targets: `--package`, else the
/// package whose directory is `start_dir`, else the only member.
pub(crate) fn select_package<'a>(
    meta: &'a MetadataOutput,
    start_dir: &Path,
    package: Option<&str>,
) -> Result<&'a PackageInfo, CliError> {
    let members: Vec<&PackageInfo> = meta
        .packages
        .iter()
        .filter(|p| meta.workspace_members.contains(&p.id))
        .collect();

    let selected = if let Some(pkg_name) = package {
        members
            .iter()
            .find(|p| p.name == pkg_name)
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
        Err(CliError::usage(format!(
            "workspace has multiple packages ({}); specify one with -p \
             or --package <NAME>",
            names.join(", ")
        )))?
    };
    Ok(selected)
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

    if matches!(command, "run" | "verify") && bin_targets.len() > 1 {
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
    Ok(exit_code(status))
}

/// Run the cargo subcommand behind `command` (see [`cargo_subcommand`])
/// for `project`, forwarding every argument after `command`. Returns the
/// child's exit code.
///
/// # Errors
/// IO error when cargo cannot start or the rustup component providing the
/// subcommand (`rustfmt`, `clippy`) is not installed.
pub fn cargo_passthrough(
    project: &ResolvedProject,
    command: &str,
    args: &[String],
) -> Result<u8, CliError> {
    let probe = Probe::system(&project.package_dir);
    let forwarded = args_after(command, args);
    let mut cmd = cargo_command(&probe, project, cargo_subcommand(command), &forwarded)?;
    let status = cmd
        .stdin(std::process::Stdio::inherit())
        .status()
        .map_err(|err| {
            CliError::Io(format!(
                "cannot run cargo {} (is cargo on PATH?): {err}",
                cargo_subcommand(command)
            ))
        })?;
    Ok(exit_code(status))
}

/// `cargo <sub> [--package P] [--manifest-path M] <forwarded>` from the
/// package directory. The package and manifest are added only when
/// `forwarded` does not choose its own.
///
/// # Errors
/// IO error when `sub` needs a rustup component that is missing.
pub(crate) fn cargo_command(
    probe: &Probe,
    project: &ResolvedProject,
    sub: &str,
    forwarded: &[String],
) -> Result<Command, CliError> {
    let probe = probe.in_dir(&project.package_dir);
    require_component(&probe, sub)?;
    let mut command = probe.cargo();
    command.arg(sub);
    // Only cargo's own options count: anything after `--` belongs to the
    // test binary, rustfmt or Clippy (`siderite test -- -p` is a filter).
    let own: Vec<&String> = forwarded.iter().take_while(|a| *a != "--").collect();
    let selects_pkg = own.iter().any(|a| {
        *a == "-p"
            || a.starts_with("-p=")
            || a.starts_with("--package")
            || *a == "--workspace"
            || *a == "--all"
    });
    if !selects_pkg && let Some(pkg) = &project.package_name {
        command.arg("--package").arg(pkg);
    }
    let has_manifest = own.iter().any(|a| a.starts_with("--manifest-path"));
    if !has_manifest && let Some(manifest) = &project.manifest_path {
        command.arg("--manifest-path").arg(manifest);
    }
    command.args(forwarded);
    Ok(command)
}

/// Fail with an install hint when `cargo <sub>` comes from a missing
/// rustup component.
fn require_component(probe: &Probe, sub: &str) -> Result<(), CliError> {
    let Some(component) = component_for(sub) else {
        return Ok(());
    };
    let output = probe.cargo().args([sub, "--version"]).output();
    match output {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let hint = if probe.on_path("rustup") {
                format!("install it with `rustup component add {component}`")
            } else {
                format!("install {component} with the package manager that provided Rust")
            };
            Err(CliError::Io(format!(
                "cargo {sub} is not available ({}); {hint}",
                first_line(&String::from_utf8_lossy(&out.stderr))
            )))
        }
        Err(err) => Err(CliError::Io(format!(
            "cannot run cargo {sub} (is cargo on PATH?): {err}"
        ))),
    }
}

/// A child's exit status as a process exit code; signals map to `1`.
pub(crate) fn exit_code(status: std::process::ExitStatus) -> u8 {
    u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)
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
    fn lint_runs_clippy_and_other_commands_keep_their_name() {
        assert_eq!(cargo_subcommand("lint"), "clippy");
        for cmd in ["build", "test", "fmt", "clean"] {
            assert_eq!(cargo_subcommand(cmd), cmd);
        }
    }

    fn stub_project(dir: &Path) -> ResolvedProject {
        ResolvedProject {
            package_dir: dir.to_path_buf(),
            manifest_path: Some(dir.join("Cargo.toml")),
            package_name: Some("demo".into()),
            binary_name: None,
        }
    }

    /// A stub cargo that prints its arguments; `fmt` is not installed.
    #[cfg(unix)]
    fn stub_probe(tag: &str) -> (Probe, PathBuf) {
        let bin = crate::toolchain::tests::stub_bin(
            tag,
            &[(
                "cargo",
                "if [ \"$2\" = --version ]; then\n\
                 [ \"$1\" = fmt ] && { echo 'error: no such command: `fmt`' >&2; exit 1; }\n\
                 echo ok; exit 0; fi\necho \"$@\"; exit 3",
            )],
        );
        (Probe::with_path(&bin, &bin), bin)
    }

    #[cfg(unix)]
    fn stdout_of(mut cmd: Command) -> (String, Option<i32>) {
        let out = cmd.output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).trim().to_owned(),
            out.status.code(),
        )
    }

    #[cfg(unix)]
    #[test]
    fn passthrough_preserves_argument_boundaries_and_status() {
        let (probe, dir) = stub_probe("lint");
        let project = stub_project(&dir);
        let forwarded: Vec<String> = ["--all-targets", "--", "-D", "warnings"]
            .iter()
            .map(|a| (*a).to_owned())
            .collect();
        let cmd = cargo_command(&probe, &project, "clippy", &forwarded).unwrap();
        let (out, code) = stdout_of(cmd);
        let manifest = dir.join("Cargo.toml");
        assert_eq!(
            out,
            format!(
                "clippy --package demo --manifest-path {} --all-targets -- -D warnings",
                manifest.display()
            )
        );
        assert_eq!(code, Some(3));

        // A package chosen after the command wins over the resolved one.
        let own = vec!["-p=other".to_owned()];
        let (out, _) = stdout_of(cargo_command(&probe, &project, "clean", &own).unwrap());
        assert!(out.starts_with("clean --manifest-path"), "{out}");
        assert!(out.ends_with("-p=other"), "{out}");

        // `--workspace` selects packages itself.
        let ws = vec!["--workspace".to_owned()];
        let (out, _) = stdout_of(cargo_command(&probe, &project, "clean", &ws).unwrap());
        assert!(!out.contains("--package"), "{out}");

        // `-p` after `--` is for the test binary, not cargo.
        let filter: Vec<String> = ["--", "-p"].iter().map(|a| (*a).to_owned()).collect();
        let (out, _) = stdout_of(cargo_command(&probe, &project, "test", &filter).unwrap());
        assert!(out.starts_with("test --package demo"), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn missing_rustfmt_is_an_actionable_error() {
        let (probe, dir) = stub_probe("fmt");
        let with_rustup = crate::toolchain::tests::stub_bin(
            "fmt-rustup",
            &[
                ("cargo", "echo 'error: no such command: `fmt`' >&2; exit 1"),
                ("rustup", "exit 0"),
            ],
        );
        let rp = Probe::with_path(&with_rustup, &with_rustup);
        let err = cargo_command(&rp, &stub_project(&dir), "fmt", &[]).unwrap_err();
        assert!(
            err.to_string().contains("rustup component add rustfmt"),
            "{err}"
        );
        let err = cargo_command(&probe, &stub_project(&dir), "fmt", &[]).unwrap_err();
        let text = err.to_string();
        // The stub PATH has no rustup.
        assert!(text.contains("package manager"), "{text}");
        assert!(text.contains("no such command"), "{text}");
        assert_eq!(err.exit_code(), 1);
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
