//! `siderite upgrade --to VERSION --dry-run [--json]`: what upgrading the
//! framework to an explicit version would change, without writing any
//! file.
//!
//! For each framework dependency (`siderite`, `siderite-cli`,
//! `siderite-testkit`) the report names its source and the manifest to
//! edit: a registry requirement becomes the new version, a git source the
//! release tag `vVERSION`, a path source cannot be changed by siderite,
//! and an inherited (`workspace = true`) dependency is edited in the
//! workspace root. Compatibility combines what semver allows with the
//! release notes this CLI carries; when the CLI has no notes for the
//! target, compatibility is reported as unknown. The report ends with
//! the commands that verify the upgrade. Rewriting files is out of
//! scope: only `--dry-run` exists.

use crate::docs::{index, version};
use crate::error::CliError;
use crate::report::text::diagnostic_line;
use crate::report::{Diagnostic, Envelope, Severity, channel};
use crate::target::{self, ProjectArgs};
use crate::workspace;
use serde::Serialize;
use std::fmt::Write;
use std::path::Path;

mod notes;
mod plan;
#[cfg(test)]
mod tests;

use notes::Semver;
use plan::{DependencyChange, Manifests, SourceKind};

const USAGE: &str = "usage: upgrade --to VERSION --dry-run [--json]";

type Found<T> = Result<T, CliError>;

/// Parsed arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    /// The target version, without a leading `v`.
    pub to: String,
    /// `--json`.
    pub json: bool,
}

/// Parse the arguments after the project flags (`upgrade ...`).
///
/// Errors:
///   A usage error without `--dry-run`, without a valid `--to`, or for
///   an unknown flag.
pub(crate) fn parse(raw: &[String]) -> Found<Options> {
    let mut args = raw.iter().skip_while(|a| *a != "upgrade").skip(1);
    let (mut to, mut dry_run, mut json) = (None, false, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--json" => json = true,
            "--to" => to = args.next().cloned(),
            other => {
                let msg = format!("unexpected argument `{other}`; {USAGE}");
                return Err(CliError::usage(msg));
            }
        }
    }
    if !dry_run {
        let what = "upgrade only previews; nothing is rewritten";
        let msg = format!("{what}. {USAGE}");
        return Err(CliError::usage(msg));
    }
    let to = to.ok_or_else(|| CliError::usage(USAGE))?;
    let to = to.trim_start_matches('v').to_owned();
    if notes::parse(&to).is_none() {
        let msg = format!("`{to}` is not a version (MAJOR.MINOR.PATCH)");
        return Err(CliError::usage(msg));
    }
    Ok(Options { to, json })
}

/// The compatibility assessment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Compatibility {
    /// What semver allows.
    pub semver: Semver,
    /// Whether this CLI carries release notes for the target.
    pub notes_known: bool,
    /// The notes that call for attention (changed, removed, deprecated,
    /// security), when known.
    pub notes: Vec<String>,
}

/// The `data` of an `upgrade` envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UpgradeData {
    /// The siderite versions locked now.
    pub from: Vec<String>,
    /// The target version.
    pub to: String,
    /// Always `true`: nothing was written.
    pub dry_run: bool,
    /// One change per framework dependency.
    pub dependencies: Vec<DependencyChange>,
    /// What to expect.
    pub compatibility: Compatibility,
    /// Commands that verify the upgrade, in order.
    pub verify: Vec<String>,
}

/// The verification commands, in order.
fn steps() -> Vec<String> {
    [
        "siderite openapi export --output openapi-before.json (before \
         editing)",
        "edit the dependencies as listed",
        "siderite build (updates Cargo.lock)",
        "siderite verify",
        "siderite makemigrations --dry-run (model changes need a \
         migration)",
        "siderite openapi export --output openapi-after.json",
        "siderite openapi diff openapi-before.json openapi-after.json",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// The inputs of a report.
pub(crate) struct Project<'a> {
    /// Locked siderite versions.
    pub locked: Vec<String>,
    /// Normal dependencies as `cargo metadata` lists them.
    pub specs: &'a [serde_json::Value],
    /// The manifests involved.
    pub manifests: Manifests<'a>,
}

/// The report for `project` and `to`, with release notes from `index`.
pub(crate) fn report(
    project: &Project<'_>,
    to: &str,
    index: &index::Index,
) -> Envelope<UpgradeData> {
    let dependencies: Vec<DependencyChange> = project
        .specs
        .iter()
        .filter_map(|spec| plan::change(spec, to, project.manifests))
        .collect();
    let from = project.locked.first().map_or("", String::as_str);
    let semver = notes::semver(from, to);
    let found = notes::notes(index, to);
    let compatibility = Compatibility {
        semver,
        notes_known: found.is_some(),
        notes: found.unwrap_or_default(),
    };
    let mut diags = Vec::new();
    if !compatibility.notes_known {
        let msg = format!(
            "this CLI (siderite {}) has no release notes for {to}; \
             compatibility is unknown",
            index.siderite
        );
        let hint = format!(
            "run the preview with siderite-cli {to} for its notes, and \
             rely on the verification steps"
        );
        let diag = Diagnostic::new("upgrade.W001", Severity::Warning, &msg);
        diags.push(diag.with_hint(&hint));
    }
    for dep in &dependencies {
        if dep.source == SourceKind::Path {
            let msg = format!(
                "{} comes from a path; check out v{to} there by hand",
                dep.name
            );
            let warn = Severity::Warning;
            diags.push(Diagnostic::new("upgrade.W002", warn, &msg));
        }
    }
    if dependencies.is_empty() {
        let msg = "the package does not depend on siderite";
        diags.push(Diagnostic::new("upgrade.W003", Severity::Warning, msg));
    }
    let data = UpgradeData {
        from: project.locked.clone(),
        to: to.to_owned(),
        dry_run: true,
        dependencies,
        compatibility,
        verify: steps(),
    };
    Envelope::new("upgrade", data, diags)
}

/// Text form.
pub(crate) fn render(report: &Envelope<UpgradeData>) -> String {
    let data = &report.data;
    let from = if data.from.is_empty() {
        "unlocked".to_owned()
    } else {
        data.from.join(", ")
    };
    let mut out = format!(
        "siderite {from} -> {} (dry run; nothing is written)\n",
        data.to
    );
    let _ = writeln!(out, "dependencies:");
    for dep in &data.dependencies {
        let proposed = dep.proposed.as_deref().unwrap_or("change by hand");
        let mut source = format!("{:?}", dep.source).to_lowercase();
        if dep.inherited {
            source.push_str(", inherited");
        }
        let _ = writeln!(
            out,
            "  {} ({source}, {}): {} -> {proposed}",
            dep.name, dep.manifest, dep.current
        );
    }
    let c = &data.compatibility;
    let semver = format!("{:?}", c.semver).to_lowercase();
    let _ = writeln!(out, "compatibility: semver {semver}");
    for note in &c.notes {
        let _ = writeln!(out, "  - {note}");
    }
    let _ = writeln!(out, "then:");
    for (n, step) in data.verify.iter().enumerate() {
        let _ = writeln!(out, "  {}. {step}", n + 1);
    }
    for diag in &report.diagnostics {
        let _ = writeln!(out, "{}", diagnostic_line(diag));
    }
    out
}

/// `siderite upgrade` from `cwd`.
///
/// Errors:
///   Usage errors, a missing package, or an IO error.
pub(crate) fn run(raw: &[String], cwd: &Path, p: &ProjectArgs) -> Found<u8> {
    let options = parse(raw)?;
    let app = target::locate("cargo", cwd, p, false)?;
    let missing = || CliError::usage("upgrade needs a Cargo package");
    let app = app.ok_or_else(missing)?;
    let ws = workspace::load("cargo", &app.dir, None)?;
    let dir = app.dir.canonicalize().unwrap_or_else(|_| app.dir.clone());
    let package = ws.packages.iter().find(|p| p.canonical_dir() == dir);
    let package = package.ok_or_else(missing)?;
    let text = std::fs::read_to_string(&package.manifest_path);
    let text = text.unwrap_or_default();
    let package_manifest = package.manifest_path.display().to_string();
    let root = ws.root.as_ref().map(|r| r.join("Cargo.toml"));
    let root = root.map_or_else(String::new, |r| r.display().to_string());
    let project = Project {
        locked: version::installed(&app.dir),
        specs: &package.specs,
        manifests: Manifests {
            text: &text,
            package: &package_manifest,
            root: &root,
        },
    };
    let packaged = index::packaged().map_err(CliError::Io)?;
    let report = report(&project, &options.to, &packaged);
    if options.json {
        channel::emit_json(&report, None)?;
    } else {
        print!("{}", render(&report));
    }
    Ok(report.exit_code())
}
