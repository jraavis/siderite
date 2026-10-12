//! A supported MCP client (the official Rust SDK's) against `siderite
//! mcp --app` in an example application: it lists the tools and reads
//! the app's documented routes through the application bridge.
//!
//! Slow (it builds the example), so it is ignored by default.

#![cfg(feature = "mcp")]
#![allow(clippy::unwrap_used)]

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use std::path::Path;
use std::process::Stdio;

#[tokio::test]
#[ignore = "slow: builds an example application"]
async fn a_client_lists_tools_and_reads_routes() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent().unwrap().parent().unwrap();
    let example = repo.join("examples/hello_world");
    let command = tokio::process::Command::new(env!("CARGO_BIN_EXE_siderite"));
    let command = command.configure(|cmd| {
        cmd.args(["mcp", "--app"]).current_dir(&example);
    });
    let builder = TokioChildProcess::builder(command).stderr(Stdio::null());
    let (transport, _) = builder.spawn().unwrap();
    let client = ().serve(transport).await.unwrap();
    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(names.contains(&"project_routes"), "{names:?}");
    for tool in &tools {
        let hints = tool.annotations.as_ref().unwrap();
        assert_eq!(hints.read_only_hint, Some(true), "{}", tool.name);
    }
    let call = CallToolRequestParams::new("project_routes");
    let result = client.call_tool(call).await.unwrap();
    let content = result.structured_content.unwrap();
    let text = content.to_string();
    assert!(text.contains("/hello/{name}"), "{text}");
    client.cancel().await.unwrap();
}
