//! Toolchain probes: find programs on a `PATH`, run `--version` with a
//! bound, and compare Rust versions.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Bound on one `--version` probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// A `major.minor.patch` Rust version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// Parse `1.99`, `1.99.0` or `1.100.0-nightly`; missing parts are 0.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().split('.');
        let mut next = |required: bool| -> Option<u64> {
            match parts.next() {
                Some(part) => {
                    let end = part
                        .find(|c: char| !c.is_ascii_digit())
                        .unwrap_or(part.len());
                    part[..end].parse().ok()
                }
                None if required => None,
                None => Some(0),
            }
        };
        Some(Self(next(true)?, next(true)?, next(false)?))
    }

    /// The version in `rustc 1.99.0 (hash date)`-style output: the first
    /// token that parses.
    pub(crate) fn from_output(output: &str) -> Option<Self> {
        output.split_whitespace().find_map(Self::parse)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// The executable `program` in the directories of `path`, if any.
pub(crate) fn which(program: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .flat_map(|dir| candidates(&dir, program))
        .find(|candidate| is_executable(candidate))
}

fn candidates(dir: &Path, program: &str) -> Vec<PathBuf> {
    let mut found = vec![dir.join(program)];
    if cfg!(windows) {
        let default = || OsString::from(".EXE;.CMD;.BAT");
        let exts = std::env::var_os("PATHEXT").unwrap_or_else(default);
        for ext in exts.to_string_lossy().split(';') {
            found.push(dir.join(format!("{program}{}", ext.to_lowercase())));
        }
    }
    found
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Run `program args` in `dir` with `path` as `PATH`; its trimmed stdout
/// when it exits successfully within the probe bound.
pub(crate) fn version_output(
    program: &Path,
    args: &[&str],
    dir: &Path,
    path: &OsStr,
) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < PROBE_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_orders_versions() {
        assert_eq!(Version::parse("1.99"), Some(Version(1, 99, 0)));
        assert_eq!(Version::parse("1.99.2"), Some(Version(1, 99, 2)));
        let nightly = Version::parse("1.100.0-nightly");
        assert_eq!(nightly, Some(Version(1, 100, 0)));
        assert_eq!(Version::parse("rustc"), None);
        assert_eq!(Version::parse("1"), None);
        let out = "rustc 1.99.0 (b940084d7 2026-09-28)";
        assert_eq!(Version::from_output(out), Some(Version(1, 99, 0)));
        assert!(Version(1, 100, 0) > Version(1, 99, 9));
        assert_eq!(Version(1, 99, 0).to_string(), "1.99.0");
    }

    #[test]
    fn which_ignores_empty_and_missing_entries() {
        let path = OsString::from("");
        assert_eq!(which("cargo", &path), None);
    }
}
