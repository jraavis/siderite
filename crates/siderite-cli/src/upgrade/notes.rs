//! Compatibility between two versions: what semver allows, and the
//! release notes this CLI carries for the target.

use crate::docs::index::{Index, Kind};
use serde::Serialize;

/// The compatibility verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Semver {
    /// Same release line: no breaking changes are allowed.
    Compatible,
    /// A new major (or `0.x` minor) line: breaking changes are allowed.
    Breaking,
    /// To an older version.
    Downgrade,
    /// A version that does not parse.
    Unknown,
}

/// `major.minor.patch`, ignoring pre-release and build parts.
pub(crate) fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.trim_start_matches(['v', '=', '^', '~']);
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(str::parse::<u64>);
    let major = parts.next()?.ok()?;
    let minor = parts.next().unwrap_or(Ok(0)).ok()?;
    let patch = parts.next().unwrap_or(Ok(0)).ok()?;
    Some((major, minor, patch))
}

/// What semver says about going from `from` to `to`.
pub(crate) fn semver(from: &str, to: &str) -> Semver {
    let (Some(a), Some(b)) = (parse(from), parse(to)) else {
        return Semver::Unknown;
    };
    if b < a {
        return Semver::Downgrade;
    }
    let line = |v: (u64, u64, u64)| if v.0 == 0 { (0, v.1) } else { (v.0, 0) };
    if line(a) == line(b) {
        Semver::Compatible
    } else {
        Semver::Breaking
    }
}

/// Changelog sections that call for attention.
const ATTENTION: [&str; 4] = [
    "### Changed",
    "### Removed",
    "### Deprecated",
    "### Security",
];

/// The notes `index` carries for `to`: the bullet points of its
/// changed, removed, deprecated and security sections; `None` when the
/// index has no section for `to`.
pub(crate) fn notes(index: &Index, to: &str) -> Option<Vec<String>> {
    let id = format!("changelog#{}", to.trim_start_matches('v'));
    let entry = index
        .entries
        .iter()
        .find(|e| e.kind == Kind::Changelog && e.id == id)?;
    let mut out = Vec::new();
    let mut wanted = false;
    for line in entry.text.lines() {
        if line.starts_with("### ") {
            wanted = ATTENTION.iter().any(|h| line.starts_with(h));
        } else if wanted && line.starts_with("- ") {
            out.push(line[2..].trim().to_owned());
        } else if wanted
            && line.starts_with("  ")
            && let Some(last) = out.last_mut()
        {
            last.push(' ');
            last.push_str(line.trim());
        }
    }
    Some(out)
}
