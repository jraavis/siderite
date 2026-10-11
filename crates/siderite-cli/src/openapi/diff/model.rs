//! Findings, JSON pointers into either document and `$ref` resolution.

use serde::Serialize;
use serde_json::Value;

/// What a change means for existing clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Impact {
    /// Existing clients can fail.
    Breaking,
    /// Not compared, or not decidable: treat as incompatible.
    Unknown,
    /// Existing clients keep working.
    Additive,
}

/// One difference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Change {
    /// What it means for clients.
    pub impact: Impact,
    /// Stable identifier, `operation_removed`.
    pub kind: &'static str,
    /// `METHOD /path`, when the change is inside an operation.
    pub operation: Option<String>,
    /// One line.
    pub message: String,
    /// Where it is in BASE (a JSON pointer), when it is there.
    pub base: Option<String>,
    /// Where it is in CURRENT (a JSON pointer), when it is there.
    pub current: Option<String>,
}

/// A value and its JSON pointer in its document.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Node<'a> {
    /// The value.
    pub value: &'a Value,
    /// Where it is.
    pub at: &'a str,
}

/// JSON pointers into BASE and CURRENT.
pub(crate) type Pointers = (String, String);

/// The same thing in BASE (`b`) and in CURRENT (`c`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pair<'a> {
    /// In BASE.
    pub b: Node<'a>,
    /// In CURRENT.
    pub c: Node<'a>,
}

impl<'a> Pair<'a> {
    /// Both nodes at their `key` member, when both have one.
    pub(crate) fn get(self, key: &str, at: &'a Pointers) -> Option<Self> {
        let b = self.b.value.get(key)?;
        let c = self.c.value.get(key)?;
        let b = Node {
            value: b,
            at: &at.0,
        };
        let c = Node {
            value: c,
            at: &at.1,
        };
        Some(Pair { b, c })
    }

    /// The pointers of both nodes' `key` member.
    pub(crate) fn child(self, key: &str) -> Pointers {
        (child(self.b.at, key), child(self.c.at, key))
    }
}

/// A JSON pointer with `segment` appended (`~` and `/` escaped).
pub(crate) fn child(at: &str, segment: &str) -> String {
    let escaped = segment.replace('~', "~0").replace('/', "~1");
    format!("{at}/{escaped}")
}

/// Why a `$ref` was not followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unresolved {
    /// It points outside the document (never fetched).
    External(String),
    /// It points at nothing, or into a loop of references.
    Missing(String),
}

/// Most `$ref` hops followed for one value.
const MAX_HOPS: usize = 16;

/// `value` with local `$ref`s followed, and the pointer it came from.
///
/// Errors:
///   [`Unresolved`] for external, missing or looping references.
pub(crate) fn resolve<'a>(
    doc: &'a Value,
    value: &'a Value,
    at: &str,
) -> Result<(&'a Value, String), Unresolved> {
    let (mut value, mut at) = (value, at.to_owned());
    for _ in 0..MAX_HOPS {
        let Some(target) = value.get("$ref").and_then(Value::as_str) else {
            return Ok((value, at));
        };
        let Some(pointer) = target.strip_prefix('#') else {
            return Err(Unresolved::External(target.to_owned()));
        };
        let found = doc.pointer(pointer);
        value = found.ok_or_else(|| Unresolved::Missing(target.to_owned()))?;
        at = pointer.to_owned();
    }
    Err(Unresolved::Missing(at))
}
