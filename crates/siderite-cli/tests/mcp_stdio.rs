//! `siderite mcp` as a process: stdout carries only JSON-RPC messages and
//! closing stdin ends it with exit status 0.

#![cfg(feature = "mcp")]
#![allow(clippy::unwrap_used)]

use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn stdout_is_protocol_only_and_eof_shuts_down() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_siderite"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let messages = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": { "name": "t", "version": "0" } } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "about", "arguments": {} } }),
    ];
    for message in messages {
        writeln!(stdin, "{message}").unwrap();
    }
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let mut ids = Vec::new();
    let mut lines = stdout.lines();
    while ids.len() < 3 {
        let line = lines.next().unwrap().unwrap();
        let message: serde_json::Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("not JSON-RPC on stdout: {line}: {e}"));
        assert_eq!(message["jsonrpc"], "2.0", "{line}");
        if let Some(id) = message.get("id") {
            ids.push(id.clone());
        }
    }
    // Requests are answered concurrently, so in any order.
    ids.sort_by_key(|id| id.as_u64());
    assert_eq!(ids, [1, 2, 3]);
    drop(stdin);
    let rest: Vec<String> = lines.map(Result::unwrap).collect();
    assert!(rest.is_empty(), "{rest:?}");
    assert!(child.wait().unwrap().success());
}
