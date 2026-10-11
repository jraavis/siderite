//! `cargo build` with machine-readable output: rendered diagnostics go to
//! stderr as cargo would print them, and the executable of the selected
//! binary is taken from the artifact messages instead of being guessed.

use super::process::{self, Group, KillOnDrop};
use crate::cargo_msg::{self, Artifact, CargoMessage, CompilerDiagnostic};
use crate::error::CliError;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// What to build.
#[derive(Debug, Clone)]
pub(crate) struct BuildSpec {
    /// The `cargo` program.
    pub cargo: PathBuf,
    /// Package directory.
    pub dir: PathBuf,
    /// `--bin`, when the package has several binaries.
    pub bin: Option<String>,
}

/// The result of one build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Built {
    /// The build succeeded; the binary to run.
    Ready(PathBuf),
    /// Compilation failed; diagnostics were printed.
    Failed,
}

/// Run the build, printing diagnostics to stderr.
///
/// Errors:
///   [`CliError::Io`] when cargo cannot be started, or a successful build
///   names no runnable binary.
pub(crate) async fn build(spec: &BuildSpec) -> Result<Built, CliError> {
    let mut command = Command::new(&spec.cargo);
    command
        .args(["build", "--message-format=json-diagnostic-rendered-ansi"])
        .current_dir(&spec.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    process::own(&mut command);
    if let Some(bin) = &spec.bin {
        command.arg("--bin").arg(bin);
    }
    let mut child = command
        .spawn()
        .map_err(|err| crate::workspace::spawn_error("cargo build", &err))?;
    // Dropping this future (shutdown) kills cargo and its rustc children.
    let mut guard = child.id().map(|pid| KillOnDrop::new(Group::led_by(pid)));
    let mut exes = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match message(&line) {
                Message::Rendered(text) => eprint!("{text}"),
                Message::Executable(name, path) => exes.push((name, path)),
                Message::Other => {}
            }
        }
    }
    let status = child
        .wait()
        .await
        .map_err(|err| CliError::Io(format!("cargo build: {err}")))?;
    if let Some(guard) = guard.as_mut() {
        guard.disarm();
    }
    if !status.success() {
        return Ok(Built::Failed);
    }
    pick(exes, spec.bin.as_deref(), &spec.dir).map(Built::Ready)
}

/// One line of cargo's JSON output, as far as the build cares.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Message {
    /// A compiler diagnostic, rendered for a terminal.
    Rendered(String),
    /// A binary target was linked: `(target name, executable)`.
    Executable(String, PathBuf),
    /// Anything else (build scripts, libraries, the final status).
    Other,
}

/// Classify one JSON line; non-JSON lines are ignored.
pub(crate) fn message(line: &str) -> Message {
    match cargo_msg::parse(line) {
        CargoMessage::Compiler(diag) => rendered(diag),
        CargoMessage::Artifact(artifact) => executable(artifact),
        CargoMessage::Other | CargoMessage::Text => Message::Other,
    }
}

fn rendered(diag: CompilerDiagnostic) -> Message {
    diag.rendered.map_or(Message::Other, Message::Rendered)
}

fn executable(artifact: Artifact) -> Message {
    let is_bin = artifact.target.kind.iter().any(|k| k == "bin");
    match (is_bin, artifact.executable) {
        (true, Some(exe)) => Message::Executable(artifact.target.name, exe),
        _ => Message::Other,
    }
}

/// `(target name, executable)` of a linked binary.
type Exe = (String, PathBuf);
type Picked = Result<PathBuf, CliError>;

/// The executable to run: the named binary, else the only one.
fn pick(exes: Vec<Exe>, bin: Option<&str>, dir: &Path) -> Picked {
    let mut matching: Vec<PathBuf> = exes
        .into_iter()
        .filter(|(name, _)| bin.is_none_or(|b| b == name))
        .map(|(_, path)| path)
        .collect();
    matching.dedup();
    match matching.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(CliError::Io(format!(
            "cargo build in {} produced no binary to run",
            dir.display()
        ))),
        _ => Err(CliError::usage(
            "the package has several binaries; pass --bin NAME",
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn classifies_cargo_messages() {
        let rendered = r#"{"reason":"compiler-message","message":
            {"level":"error","message":"x","rendered":"error[E0425]: x\n"}}"#;
        let rendered = rendered.replace('\n', " ");
        assert_eq!(
            message(&rendered),
            Message::Rendered("error[E0425]: x\n".into())
        );
        let artifact = r#"{"reason":"compiler-artifact","target":
            {"kind":["bin"],"name":"app"},"executable":"/t/debug/app"}"#;
        let artifact = artifact.replace('\n', " ");
        assert_eq!(
            message(&artifact),
            Message::Executable("app".into(), "/t/debug/app".into())
        );
        let lib = r#"{"reason":"compiler-artifact","target":
            {"kind":["lib"],"name":"x"},"executable":null}"#;
        assert_eq!(message(&lib.replace('\n', " ")), Message::Other);
        assert_eq!(message("Compiling app"), Message::Other);
    }

    #[test]
    fn picks_the_selected_binary() {
        let dir = Path::new("/p");
        let exes = || {
            vec![
                ("a".to_owned(), PathBuf::from("/t/a")),
                ("b".to_owned(), PathBuf::from("/t/b")),
            ]
        };
        let picked = pick(exes(), Some("b"), dir).unwrap();
        assert_eq!(picked, PathBuf::from("/t/b"));
        assert_eq!(pick(exes(), None, dir).unwrap_err().exit_code(), 2);
        assert_eq!(pick(Vec::new(), None, dir).unwrap_err().exit_code(), 1);
    }
}
