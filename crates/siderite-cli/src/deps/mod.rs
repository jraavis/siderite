//! `siderite add` and `siderite remove`: dependency edits through
//! `cargo add` / `cargo remove`, for the selected package.
//!
//! Cargo owns every manifest edit, so workspace inheritance
//! (`workspace = true`), path and git sources and formatting are kept as
//! Cargo keeps them. The package is always named (`-p`). Crate names and
//! presets are separate: `add serde` passes through, `add --preset
//! postgres` expands to the feature change the preset stands for, and the
//! two do not mix in one call. Each Cargo command is shown before it runs;
//! a failure stops the rest and says what was already applied, since
//! nothing is rolled back.

use crate::error::CliError;
use crate::target::{self, ProjectArgs};
use std::path::Path;
use std::process::Command;

type Planned = Result<Vec<Vec<String>>, CliError>;
type Done = Result<(), CliError>;
type Code = Result<u8, CliError>;

const NEEDS_PACKAGE: &str = "this command needs a Cargo package; cd into one";

/// A named dependency change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Preset {
    /// The name after `--preset`.
    pub name: &'static str,
    /// The dependency it changes; it must already be a dependency, so its
    /// source is kept.
    pub dependency: &'static str,
    /// The feature it enables there.
    pub feature: &'static str,
}

/// Every preset: only mappings the CLI's own features support. Redis
/// and MongoDB are set up in the app, so they have none.
pub(crate) const PRESETS: &[Preset] = &[
    Preset {
        name: "mysql",
        dependency: "siderite-cli",
        feature: "mysql",
    },
    Preset {
        name: "postgres",
        dependency: "siderite-cli",
        feature: "postgres",
    },
];

/// `add` or `remove`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edit {
    /// `cargo add`.
    Add,
    /// `cargo remove`.
    Remove,
}

impl Edit {
    const fn cargo(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Remove => "remove",
        }
    }
}

/// Parsed arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Request {
    /// `add` or `remove`.
    pub edit: Edit,
    /// Presets named with `--preset`.
    pub presets: Vec<&'static Preset>,
    /// Everything else, for Cargo.
    pub cargo_args: Vec<String>,
    /// `--dry-run` / `-n`, which Cargo also receives.
    pub dry_run: bool,
}

/// Flags that are not crate names (they may come with presets).
const SHARED: &[&str] = &["--dry-run", "-n", "--offline", "--locked"];

/// Parse the arguments after the project flags.
///
/// Errors:
///   A usage error for nothing to do, an unknown preset, or presets mixed
///   with other Cargo arguments.
pub(crate) fn parse(raw: &[String]) -> Result<Request, CliError> {
    let mut words = raw.iter();
    let edit = match words.next().map(String::as_str) {
        Some("add") => Edit::Add,
        Some("remove") => Edit::Remove,
        _ => return Err(CliError::usage(USAGE)),
    };
    let mut request = Request {
        edit,
        presets: Vec::new(),
        cargo_args: Vec::new(),
        dry_run: false,
    };
    let mut mixed = false;
    while let Some(arg) = words.next() {
        let named = match arg.strip_prefix("--preset=") {
            Some(name) => Some(name.to_owned()),
            None if arg == "--preset" => words.next().cloned(),
            None => None,
        };
        if arg == "--preset" || arg.starts_with("--preset=") {
            let name = named.ok_or_else(|| CliError::usage(USAGE))?;
            request.presets.push(preset(&name)?);
            continue;
        }
        request.dry_run |= arg == "--dry-run" || arg == "-n";
        mixed |= !SHARED.contains(&arg.as_str());
        request.cargo_args.push(arg.clone());
    }
    if !request.presets.is_empty() && mixed {
        let msg = "--preset cannot be combined with crate names or other \
                   cargo options; run them separately";
        return Err(CliError::usage(msg));
    }
    if request.presets.is_empty() && !mixed {
        return Err(CliError::usage(USAGE));
    }
    Ok(request)
}

const USAGE: &str = "usage: add CRATE... [cargo add options] | add --preset \
     postgres|mysql [--dry-run]\n       remove CRATE... [cargo remove \
     options] | remove --preset NAME";

fn preset(name: &str) -> Result<&'static Preset, CliError> {
    PRESETS.iter().find(|p| p.name == name).ok_or_else(|| {
        let known: Vec<&str> = PRESETS.iter().map(|p| p.name).collect();
        CliError::usage(format!(
            "unknown preset `{name}`; presets: {}",
            known.join(", ")
        ))
    })
}

/// The Cargo argument lists to run for `request` on `package`.
///
/// Errors:
///   [`CliError::Conflict`] for `remove --preset`: Cargo cannot remove one
///   feature, and removing the dependency would discard features the
///   preset did not add, so the manual edit is described instead.
pub(crate) fn commands(request: &Request, package: &str) -> Planned {
    let head = |edit: Edit| {
        let words = [edit.cargo(), "--package", package];
        words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>()
    };
    if request.presets.is_empty() {
        let mut command = head(request.edit);
        command.extend(request.cargo_args.iter().cloned());
        return Ok(vec![command]);
    }
    if request.edit == Edit::Remove {
        return Err(CliError::Conflict(manual_removal(&request.presets)));
    }
    let shared = request.cargo_args.iter().cloned();
    let shared: Vec<String> = shared.collect();
    let commands = request.presets.iter().map(|p| {
        let mut command = head(Edit::Add);
        command.extend([p.dependency.to_owned(), "--features".to_owned()]);
        command.push(p.feature.to_owned());
        command.extend(shared.iter().cloned());
        command
    });
    Ok(commands.collect())
}

fn manual_removal(presets: &[&Preset]) -> String {
    let mut out = String::from(
        "cargo cannot remove a single feature, and removing the dependency \
         would drop features and settings the preset did not add; edit \
         Cargo.toml instead:",
    );
    for p in presets {
        out.push_str(&format!(
            "\n  remove \"{}\" from the `features` of `{}`",
            p.feature, p.dependency
        ));
    }
    out
}

/// Presets need their dependency already present, so `cargo add` only
/// changes features and never picks a new source.
///
/// Errors:
///   A usage error naming the missing dependency.
pub(crate) fn require(request: &Request, dependencies: &[String]) -> Done {
    for p in &request.presets {
        if !dependencies.iter().any(|d| d == p.dependency) {
            return Err(CliError::usage(format!(
                "preset `{}` changes `{}`, which is not a dependency of \
                 this package; add it first (`siderite add {}`)",
                p.name, p.dependency, p.dependency
            )));
        }
    }
    Ok(())
}

/// Run `commands` in order with `cargo` in `dir`, stopping at the first
/// failure.
///
/// Returns:
///   The exit code of the last command run.
///
/// Errors:
///   An IO error when cargo cannot start.
pub(crate) fn apply(dir: &Path, commands: &[Vec<String>]) -> Code {
    let mut applied: Vec<String> = Vec::new();
    for args in commands {
        let shown = format!("cargo {}", args.join(" "));
        eprintln!("siderite: {shown}");
        let status = Command::new("cargo")
            .args(args)
            .current_dir(dir)
            .status()
            .map_err(|err| crate::workspace::spawn_error(&shown, &err))?;
        if !status.success() {
            if !applied.is_empty() {
                eprintln!(
                    "siderite: `{shown}` failed after these were applied: \
                     {}; nothing was rolled back (check Cargo.toml)",
                    applied.join("; ")
                );
            }
            let code = status.code().unwrap_or(1);
            return Ok(u8::try_from(code).unwrap_or(1));
        }
        applied.push(shown);
    }
    Ok(0)
}

/// `siderite add` / `siderite remove` from `cwd`.
///
/// Errors:
///   Usage errors, a preset conflict, or an IO error starting cargo.
pub(crate) fn run(raw: &[String], cwd: &Path, project: &ProjectArgs) -> Code {
    let words = ["add", "remove"];
    let from = raw.iter().position(|a| words.contains(&a.as_str()));
    let request = parse(&raw[from.unwrap_or(raw.len())..])?;
    let found = target::locate("cargo", cwd, project, false)?;
    let found = found.ok_or_else(|| CliError::usage(NEEDS_PACKAGE))?;
    if !request.presets.is_empty() {
        let ws = crate::workspace::load("cargo", &found.dir, None)?;
        let package = ws.packages.iter().find(|p| p.name == found.package);
        let dependencies = package.map(|p| p.dependencies.as_slice());
        require(&request, dependencies.unwrap_or_default())?;
    }
    let commands = commands(&request, &found.package)?;
    apply(&found.dir, &commands)
}

#[cfg(test)]
mod tests;
