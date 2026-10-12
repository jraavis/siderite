//! The transport boundary, driven with raw JSON-RPC over an in-memory
//! pipe: negotiation, capabilities, malformed input, limits,
//! cancellation and shutdown.

#![allow(clippy::unwrap_used)]

use super::limits::Limits;
use super::server::{Server, ToolSpec};
use super::{serve, standard};
use crate::error::CliError;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::Lines;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(10);

struct Session {
    writer: DuplexStream,
    lines: Lines<BufReader<DuplexStream>>,
    server: JoinHandle<Result<(), CliError>>,
}

impl Session {
    /// One pipe each way, so closing the client's writer is end of input.
    fn start(server: Server, limits: Limits) -> Self {
        let (writer, input) = tokio::io::duplex(1 << 16);
        let (output, reader) = tokio::io::duplex(1 << 16);
        let server = tokio::spawn(serve(server, input, output, limits));
        Self {
            writer,
            lines: BufReader::new(reader).lines(),
            server,
        }
    }

    async fn send(&mut self, message: &Value) {
        self.raw(&message.to_string()).await;
    }

    async fn raw(&mut self, line: &str) {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\n');
        let _ = self.writer.write_all(&bytes).await;
    }

    /// The next message, or `None` at the end of the session.
    async fn next(&mut self) -> Option<Value> {
        let line = tokio::time::timeout(WAIT, self.lines.next_line());
        let line = line.await.unwrap().ok()??;
        Some(serde_json::from_str(&line).unwrap())
    }

    /// Messages until the response to `id`, which is returned.
    async fn response(&mut self, id: &Value) -> Value {
        loop {
            let message = self.next().await.unwrap();
            if message.get("id") == Some(id) {
                return message;
            }
        }
    }

    async fn initialize(&mut self, version: &str) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }))
        .await;
        let answer = self.response(&json!(0)).await;
        let note = json!({
            "jsonrpc": "2.0", "method": "notifications/initialized"
        });
        self.send(&note).await;
        answer
    }

    async fn call(&mut self, id: u64, tool: &str) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": tool, "arguments": {} }
        }))
        .await;
        self.response(&json!(id)).await
    }

    async fn ping(&mut self, id: u64) -> Value {
        let ping = json!({ "jsonrpc": "2.0", "id": id, "method": "ping" });
        self.send(&ping).await;
        self.response(&json!(id)).await
    }

    async fn close(self) -> Result<(), CliError> {
        drop(self.writer);
        tokio::time::timeout(WAIT, self.server)
            .await
            .unwrap()
            .unwrap()
    }
}

#[tokio::test]
async fn initialization_capabilities_and_tools() {
    let mut s = Session::start(standard(), Limits::default());
    let init = s.initialize("2025-11-25").await;
    let result = &init["result"];
    assert_eq!(result["protocolVersion"], "2025-11-25");
    assert_eq!(result["serverInfo"]["name"], "siderite");
    assert!(result["capabilities"]["tools"].is_object(), "{init}");
    for absent in ["resources", "prompts"] {
        assert!(result["capabilities"].get(absent).is_none(), "{init}");
    }
    s.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .await;
    let list = s.response(&json!(1)).await;
    let tool = &list["result"]["tools"][0];
    assert_eq!(tool["name"], "about");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    let about = s.call(2, "about").await;
    let content = &about["result"]["structuredContent"];
    assert_eq!(content["siderite"], env!("CARGO_PKG_VERSION"));
    let unknown = s.call(3, "shell").await;
    assert_eq!(unknown["error"]["code"], -32602, "{unknown}");
    s.close().await.unwrap();
}

#[tokio::test]
async fn versions_are_negotiated() {
    for (asked, answered) in [
        ("2024-11-05", Some("2024-11-05")),
        ("2025-06-18", Some("2025-06-18")),
        ("1999-01-01", None),
    ] {
        let mut s = Session::start(standard(), Limits::default());
        let init = s.initialize(asked).await;
        let version = init["result"]["protocolVersion"].as_str().unwrap();
        match answered {
            Some(want) => assert_eq!(version, want),
            // An unknown version gets one the server supports.
            None => assert!(version.starts_with("20"), "{init}"),
        }
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn malformed_messages_do_not_end_the_session() {
    let mut s = Session::start(standard(), Limits::default());
    s.initialize("2025-11-25").await;
    s.raw("{not json").await;
    s.raw("[]").await;
    s.send(&json!({ "jsonrpc": "2.0", "id": 9, "method": "no/such" }))
        .await;
    let missing = s.response(&json!(9)).await;
    assert_eq!(missing["error"]["code"], -32601, "{missing}");
    let pong = s.ping(10).await;
    assert!(pong.get("result").is_some(), "{pong}");
    s.close().await.unwrap();
}

#[tokio::test]
async fn an_oversized_message_ends_the_session() {
    let limits = Limits {
        max_message: 512,
        ..Limits::default()
    };
    let mut s = Session::start(standard(), limits);
    s.initialize("2025-11-25").await;
    let big = "x".repeat(4096);
    s.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": big }))
        .await;
    assert!(s.next().await.is_none(), "the session ends");
    let ended = tokio::time::timeout(WAIT, s.server).await.unwrap();
    let err = ended.unwrap().unwrap_err();
    assert!(err.to_string().contains("512 bytes"), "{err}");
}

fn slow_server(limits: Limits) -> Server {
    let wait = ToolSpec::new("wait", "sleeps", json!({}), |_| async {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok(json!("late"))
    });
    let big = ToolSpec::new("big", "a large result", json!({}), |_| async {
        Ok(json!("y".repeat(10_000)))
    });
    let fails = ToolSpec::new("fails", "an error", json!({}), |_| async {
        Err("no: password=hunter2".to_owned())
    });
    Server::new(vec![wait, big, fails], limits)
}

#[tokio::test]
async fn calls_are_bounded_in_time_and_size() {
    let limits = Limits {
        call_timeout: Duration::from_millis(100),
        max_result: 1000,
        ..Limits::default()
    };
    let mut s = Session::start(slow_server(limits), limits);
    s.initialize("2025-11-25").await;
    let slow = s.call(1, "wait").await;
    assert_eq!(slow["result"]["isError"], true, "{slow}");
    assert!(slow.to_string().contains("longer than"), "{slow}");
    let big = s.call(2, "big").await;
    assert!(big.to_string().contains("over the 1000 limit"), "{big}");
    let fails = s.call(3, "fails").await;
    assert!(!fails.to_string().contains("hunter2"), "{fails}");
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_cancelled_call_gets_no_answer() {
    let limits = Limits::default();
    let mut s = Session::start(slow_server(limits), limits);
    s.initialize("2025-11-25").await;
    s.send(&json!({
        "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": { "name": "wait", "arguments": {} }
    }))
    .await;
    s.send(&json!({
        "jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": { "requestId": 5 }
    }))
    .await;
    s.send(&json!({ "jsonrpc": "2.0", "id": 6, "method": "ping" }))
        .await;
    loop {
        let message = s.next().await.unwrap();
        assert_ne!(message.get("id"), Some(&json!(5)), "{message}");
        if message.get("id") == Some(&json!(6)) {
            break;
        }
    }
    // Closing stdin ends the session cleanly, the cancelled call included.
    s.close().await.unwrap();
}
