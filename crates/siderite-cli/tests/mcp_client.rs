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

/// Live migration status through the client, for an alias named on the
/// command line (`DATABASE_URL` must point at PostgreSQL).
#[tokio::test]
#[ignore = "live PostgreSQL; set DATABASE_URL"]
async fn a_client_reads_live_migration_status() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent().unwrap().parent().unwrap();
    let example = repo.join("examples/blog_postgres");
    let url = std::env::var("DATABASE_URL").unwrap();
    let command = tokio::process::Command::new(env!("CARGO_BIN_EXE_siderite"));
    let command = command.configure(|cmd| {
        let args = ["mcp", "--app", "--live", "--database", "default"];
        cmd.args(args)
            .current_dir(&example)
            .env("DATABASE_URL", &url);
    });
    let builder = TokioChildProcess::builder(command).stderr(Stdio::null());
    let (transport, _) = builder.spawn().unwrap();
    let client = ().serve(transport).await.unwrap();
    let mut call = CallToolRequestParams::new("migration_status");
    let args = serde_json::json!({ "database": "default" });
    call.arguments = args.as_object().cloned();
    let result = client.call_tool(call).await.unwrap();
    let content = result.structured_content.unwrap();
    let report = &content["report"];
    assert_eq!(report["command"], "showmigrations", "{content}");
    assert!(report["data"]["migrations"].is_array(), "{content}");
    client.cancel().await.unwrap();
}
