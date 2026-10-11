//! Cargo's machine-readable output (`--message-format=json`), shared by
//! `dev` and `verify`.
//!
//! Only stable fields of the documented format are read: compiler
//! messages (level, code, message, spans, rendered text) and artifacts.
//! Other cargo messages are [`CargoMessage::Other`]; lines that are not
//! cargo's (test binaries print to the same stdout) are
//! [`CargoMessage::Text`].

use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;

/// Version of the [`CompilerDiagnostic`] shape in reports.
pub(crate) const DIAGNOSTIC_FORMAT: u32 = 1;

/// One line of cargo's JSON output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CargoMessage {
    /// A rustc or Clippy diagnostic.
    Compiler(CompilerDiagnostic),
    /// A built target: its name, kinds and executable (binaries only).
    Artifact(Artifact),
    /// Another cargo message.
    Other,
    /// Not a cargo message.
    Text,
}

/// A compiled target.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Artifact {
    /// The target.
    pub target: Target,
    /// The executable, for binaries and tests.
    #[serde(default)]
    pub executable: Option<PathBuf>,
}

/// The target of an [`Artifact`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Target {
    /// Target name.
    pub name: String,
    /// `bin`, `lib`, `test`, ...
    #[serde(default)]
    pub kind: Vec<String>,
}

/// A compiler diagnostic as rustc reports it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct CompilerDiagnostic {
    /// `error`, `warning`, `note`, `help`, ...
    pub level: String,
    /// `E0308`, `clippy::needless_return`, ...
    #[serde(default, deserialize_with = "code")]
    pub code: Option<String>,
    /// The main message.
    pub message: String,
    /// Source locations.
    #[serde(default)]
    pub spans: Vec<Span>,
    /// The full explanation as rustc prints it (notes and help included).
    #[serde(default)]
    pub rendered: Option<String>,
}

/// A source location of a [`CompilerDiagnostic`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct Span {
    /// File, relative to the workspace root.
    #[serde(rename(deserialize = "file_name"))]
    pub file: String,
    /// First line (1-based).
    pub line_start: u32,
    /// Last line.
    pub line_end: u32,
    /// First column (1-based).
    pub column_start: u32,
    /// Column after the end.
    pub column_end: u32,
    /// Whether this is the main location.
    #[serde(rename(deserialize = "is_primary"))]
    pub primary: bool,
    /// The label at the location.
    #[serde(default)]
    pub label: Option<String>,
}

impl Span {
    /// `file:line:column` of the start.
    pub(crate) fn position(&self) -> String {
        format!("{}:{}:{}", self.file, self.line_start, self.column_start)
    }
}

/// `{"code": "E0308", "explanation": ...}` to `E0308`.
fn code<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    struct Code {
        code: String,
    }
    Ok(Option::<Code>::deserialize(d)?.map(|c| c.code))
}

#[derive(Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
enum Line {
    CompilerMessage {
        message: CompilerDiagnostic,
    },
    CompilerArtifact(Artifact),
    #[serde(other)]
    Other,
}

/// Classify one line of output.
pub(crate) fn parse(line: &str) -> CargoMessage {
    if !line.starts_with('{') {
        return CargoMessage::Text;
    }
    use CargoMessage as M;
    match serde_json::from_str(line) {
        Ok(Line::CompilerMessage { message }) => M::Compiler(message),
        Ok(Line::CompilerArtifact(artifact)) => M::Artifact(artifact),
        Ok(Line::Other) => M::Other,
        Err(_) => M::Text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ERROR: &str = r#"{"reason":"compiler-message","package_id":"p",
        "message":{"$message_type":"diagnostic","message":"mismatched types",
        "code":{"code":"E0308","explanation":"long"},"level":"error",
        "spans":[{"file_name":"src/main.rs","byte_start":1,"byte_end":2,
        "line_start":3,"line_end":3,"column_start":5,"column_end":9,
        "is_primary":true,"text":[],"label":"expected `u8`",
        "suggested_replacement":null,"expansion":null}],
        "children":[],"rendered":"error[E0308]: mismatched types\n"}}"#;

    #[test]
    fn reads_compiler_messages() {
        let line = ERROR.replace('\n', "");
        let CargoMessage::Compiler(diag) = parse(&line) else {
            panic!("not a compiler message");
        };
        assert_eq!(diag.level, "error");
        assert_eq!(diag.code.as_deref(), Some("E0308"));
        assert_eq!(diag.message, "mismatched types");
        let span = &diag.spans[0];
        assert_eq!((span.file.as_str(), span.line_start), ("src/main.rs", 3));
        assert_eq!((span.column_start, span.column_end), (5, 9));
        assert!(span.primary);
        assert_eq!(span.label.as_deref(), Some("expected `u8`"));
        let rendered = diag.rendered.as_deref();
        assert_eq!(rendered, Some("error[E0308]: mismatched types\n"));
    }

    #[test]
    fn everything_else_is_other() {
        let finished = r#"{"reason":"build-finished","success":true}"#;
        assert_eq!(parse(finished), CargoMessage::Other);
        assert_eq!(parse("test a ... ok"), CargoMessage::Text);
        assert_eq!(parse("{not json"), CargoMessage::Text);
        let no_code = r#"{"reason":"compiler-message","message":
            {"message":"m","level":"warning","code":null,"spans":[]}}"#;
        let parsed = parse(&no_code.replace('\n', ""));
        let CargoMessage::Compiler(diag) = parsed else {
            panic!("not a compiler message");
        };
        assert_eq!((diag.code, diag.rendered), (None, None));
    }
}
