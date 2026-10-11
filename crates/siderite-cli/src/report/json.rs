//! The JSON renderer.

use serde::Serialize;

/// Render `report` as exactly one compact JSON value followed by a
/// newline, ready to write to stdout. Object keys come out in struct
/// declaration order, so the output is stable.
///
/// Errors:
///   The serializer's error (not expected for the report types).
pub fn render<T: Serialize>(report: &T) -> Result<String, serde_json::Error> {
    let mut out = serde_json::to_string(report)?;
    out.push('\n');
    Ok(out)
}
