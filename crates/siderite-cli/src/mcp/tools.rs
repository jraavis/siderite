//! The tools the server offers.

use super::server::ToolSpec;
use serde_json::json;

/// Every tool, in the order clients list them.
pub(crate) fn all() -> Vec<ToolSpec> {
    vec![about()]
}

/// `about`: the server's version, limits and tools.
fn about() -> ToolSpec {
    let input = json!({ "type": "object", "properties": {} });
    ToolSpec::new(
        "about",
        "The siderite version this server describes and its limits.",
        input,
        |_| async {
            let limits = super::limits::Limits::default();
            Ok(json!({
                "siderite": env!("CARGO_PKG_VERSION"),
                "read_only": true,
                "limits": {
                    "max_message_bytes": limits.max_message,
                    "call_timeout_secs": limits.call_timeout.as_secs(),
                    "max_result_bytes": limits.max_result,
                },
            }))
        },
    )
}
