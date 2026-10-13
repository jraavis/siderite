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
    let mut s = Session::start(standard(None), Limits::default());
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
    let tools = list["result"]["tools"].as_array().unwrap();
    let names = tools.iter().filter_map(|t| t["name"].as_str());
    let names: Vec<&str> = names.collect();
    let want = [
        "about",
        "search_docs",
        "explain_diagnostic",
        "list_commands",
    ];
    assert_eq!(names, want);
    for tool in tools {
        assert_eq!(tool["annotations"]["readOnlyHint"], true, "{tool}");
    }
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
        let mut s = Session::start(standard(None), Limits::default());
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
    let mut s = Session::start(standard(None), Limits::default());
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
    let mut s = Session::start(standard(None), limits);
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

mod project {
    use super::*;
    use crate::fixtures::ScratchDir;
    use crate::mcp::bridge::{Bridge, fingerprint};
    use std::sync::Arc;

    /// A `cargo` that writes a fixed context as the app would, prints
    /// noise to stdout, and counts its runs.
    #[cfg(unix)]
    fn fake_cargo(dir: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let context = json!({ "data": {
            "snapshot": {
                "framework": { "name": "siderite", "version": "9.9.9" },
                "app": { "title": "Shop" },
                "routes": [{ "method": "GET", "path": "/items" }],
                "models": [],
                "openapi": { "openapi": "3.1.0" },
            },
            "migrations": { "state": "unknown" },
        }});
        let script = format!(
            "#!/bin/sh\necho run >> '{}/runs'\necho 'factory noise'\n\
             printf '%s' '{context}' > \"$SIDERITE_REPORT_PATH\"\n",
            dir.display()
        );
        let path = dir.join("cargo");
        std::fs::write(&path, script).unwrap();
        let mode = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&path, mode).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn project_tools_build_once_and_rebuild_after_changes() {
        let tools = ScratchDir::new("mcp-cargo");
        let app = ScratchDir::new("mcp-app");
        std::fs::create_dir(app.path().join("src")).unwrap();
        let main = app.path().join("src/main.rs");
        std::fs::write(&main, "fn main() {}").unwrap();
        let cargo = fake_cargo(tools.path());
        let bridge = Bridge::new(app.path().to_path_buf(), None);
        let bridge = Arc::new(bridge.with_cargo(cargo));
        let app = crate::mcp::tools::App {
            bridge,
            live: Vec::new(),
        };
        let server = standard(Some(&app));
        let mut s = Session::start(server, Limits::default());
        s.initialize("2025-11-25").await;
        s.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
            .await;
        let list = s.response(&json!(1)).await;
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        let want = [
            "about",
            "search_docs",
            "explain_diagnostic",
            "list_commands",
            "project_summary",
            "project_routes",
            "project_models",
            "project_openapi",
            "project_check",
        ];
        assert_eq!(names, want);
        let routes = s.call(2, "project_routes").await;
        let routes = &routes["result"]["structuredContent"];
        assert_eq!(routes["snapshot"], "rebuilt", "{routes}");
        assert_eq!(routes["data"][0]["path"], "/items");
        let summary = s.call(3, "project_summary").await;
        let summary = &summary["result"]["structuredContent"];
        assert_eq!(summary["snapshot"], "cached");
        assert_eq!(summary["data"]["routes"], 1);
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&main, "fn main() { () }").unwrap();
        let again = s.call(4, "project_openapi").await;
        let again = &again["result"]["structuredContent"];
        assert_eq!(again["snapshot"], "rebuilt");
        let runs = std::fs::read_to_string(tools.path().join("runs"));
        assert_eq!(runs.unwrap().lines().count(), 2);
        s.close().await.unwrap();
    }

    #[test]
    fn the_fingerprint_follows_sources_and_ignores_build_output() {
        let app = ScratchDir::new("mcp-print");
        std::fs::create_dir_all(app.path().join("target/debug")).unwrap();
        std::fs::write(app.path().join("Cargo.toml"), "[package]").unwrap();
        let first = fingerprint(app.path());
        let build = app.path().join("target/debug/out");
        std::fs::write(build, "binary").unwrap();
        assert_eq!(fingerprint(app.path()), first);
        std::fs::write(app.path().join("Cargo.toml"), "[package]\n").unwrap();
        assert_ne!(fingerprint(app.path()), first);
    }

    #[tokio::test]
    async fn without_an_app_there_are_no_project_tools() {
        let mut s = Session::start(standard(None), Limits::default());
        s.initialize("2025-11-25").await;
        let call = s.call(1, "project_routes").await;
        assert_eq!(call["error"]["code"], -32602, "{call}");
        s.close().await.unwrap();
    }
}

mod checks_and_live {
    use super::*;
    use crate::fixtures::ScratchDir;
    use crate::mcp::bridge::Bridge;
    use crate::mcp::parse;
    use crate::mcp::tools::{self, App};
    use std::path::Path;
    use std::sync::Arc;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn live_tools_need_the_app_live_and_aliases_together() {
        let live = ["mcp", "--app", "--live", "--database", "default"];
        let options = parse(&words(&live)).unwrap();
        assert!(options.app);
        assert_eq!(options.live, ["default"]);
        assert!(parse(&words(&["mcp"])).unwrap().live.is_empty());
        for bad in [
            &["mcp", "--live"][..],
            &["mcp", "--app", "--live"],
            &["mcp", "--app", "--database", "x"],
            &["mcp", "--live", "--database", "x"],
            &["mcp", "--write"],
        ] {
            assert!(parse(&words(bad)).is_err(), "{bad:?}");
        }
    }

    /// A `cargo` that records its arguments and writes a report, or,
    /// when a `slow` file exists, starts a child (as cargo starts rustc
    /// and the app), records the child's pid and waits for it.
    #[cfg(unix)]
    fn recording_cargo(dir: &Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let d = dir.display();
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{d}/args'\n\
             [ -f '{d}/slow' ] && {{ sleep 30 & \
             echo $! > '{d}/pid'; wait; }}\n\
             printf '%s' '{{\"ok\":true,\"data\":{{\"errors\":0}},\
             \"diagnostics\":[]}}' > \"$SIDERITE_REPORT_PATH\"\n"
        );
        let path = dir.join("cargo");
        std::fs::write(&path, script).unwrap();
        let mode = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&path, mode).unwrap();
        path
    }

    #[cfg(unix)]
    fn session(dir: &Path, live: &[&str]) -> Session {
        session_with(dir, live, Limits::default())
    }

    #[cfg(unix)]
    fn session_with(dir: &Path, live: &[&str], limits: Limits) -> Session {
        let cargo = recording_cargo(dir);
        let bridge = Bridge::new(dir.to_path_buf(), None).with_cargo(cargo);
        let app = App {
            bridge: Arc::new(bridge),
            live: live.iter().map(|a| (*a).to_owned()).collect(),
        };
        let server = Server::new(tools::all(Some(&app), limits), limits);
        Session::start(server, limits)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checks_and_status_run_exactly_the_read_commands() {
        let dir = ScratchDir::new("mcp-live");
        let mut s = session(dir.path(), &["default"]);
        s.initialize("2025-11-25").await;
        let check = s.call(1, "project_check").await;
        let check = &check["result"]["structuredContent"];
        assert_eq!(check["ok"], true, "{check}");
        s.send(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "migration_status",
                "arguments": { "database": "default" } }
        }))
        .await;
        let status = s.response(&json!(2)).await;
        let status = &status["result"]["structuredContent"];
        assert_eq!(status["database"], "default", "{status}");
        s.send(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "migration_status",
                "arguments": { "database": "replica" } }
        }))
        .await;
        let refused = s.response(&json!(3)).await;
        assert_eq!(refused["result"]["isError"], true, "{refused}");
        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        let lines: Vec<&str> = args.lines().collect();
        assert_eq!(
            lines,
            [
                "run --quiet -- --json check",
                "run --quiet -- --database default --json showmigrations",
            ]
        );
        s.close().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn without_live_there_is_no_status_tool_and_cancel_kills_work() {
        let dir = ScratchDir::new("mcp-cancel");
        let mut s = session(dir.path(), &[]);
        s.initialize("2025-11-25").await;
        let status = s.call(1, "migration_status").await;
        assert_eq!(status["error"]["code"], -32602, "{status}");
        // A slow check (the fake's child sleeps): cancel it and the
        // child ends with cargo.
        std::fs::write(dir.path().join("slow"), "").unwrap();
        s.send(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "project_check", "arguments": {} }
        }))
        .await;
        let pid_file = dir.path().join("pid");
        let pid = poll(|| std::fs::read_to_string(&pid_file).ok()).await;
        let pid = pid.trim().to_owned();
        assert!(alive(&pid), "the build runs");
        s.send(&json!({
            "jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": { "requestId": 2 }
        }))
        .await;
        poll(|| (!alive(&pid)).then_some(())).await;
        s.close().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_timed_out_build_leaves_nothing_running() {
        let dir = ScratchDir::new("mcp-timeout");
        // A cold shell can take longer than a second to record its
        // child. The child sleeps much longer than this bound.
        let limits = Limits {
            build_timeout: Duration::from_secs(5),
            ..Limits::default()
        };
        let mut s = session_with(dir.path(), &[], limits);
        s.initialize("2025-11-25").await;
        std::fs::write(dir.path().join("slow"), "").unwrap();
        s.send(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "project_check", "arguments": {} }
        }))
        .await;
        let pid_file = dir.path().join("pid");
        let pid = poll(|| std::fs::read_to_string(&pid_file).ok()).await;
        let pid = pid.trim().to_owned();
        assert!(alive(&pid), "the build runs");
        let check = s.response(&json!(1)).await;
        assert_eq!(check["result"]["isError"], true, "{check}");
        poll(|| (!alive(&pid)).then_some(())).await;
        s.close().await.unwrap();
    }

    /// Whether process `pid` runs (a zombie does not count).
    #[cfg(unix)]
    fn alive(pid: &str) -> bool {
        let out = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&out.stdout);
        let stat = stat.trim();
        !stat.is_empty() && !stat.starts_with('Z')
    }

    /// `check()`'s first `Some`, polling for up to ten seconds.
    #[cfg(unix)]
    async fn poll<T>(mut check: impl FnMut() -> Option<T>) -> T {
        for _ in 0..200 {
            if let Some(found) = check() {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting");
    }
}

mod docs_and_diagnostics {
    use super::*;

    async fn call_with(s: &mut Session, id: u64, t: &str, a: Value) -> Value {
        s.send(&json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": t, "arguments": a }
        }))
        .await;
        let answer = s.response(&json!(id)).await;
        answer["result"].clone()
    }

    #[tokio::test]
    async fn results_match_the_cli_reports() {
        let mut s = Session::start(standard(None), Limits::default());
        s.initialize("2025-11-25").await;
        let args = json!({ "query": "transaction", "limit": 2 });
        let found = call_with(&mut s, 1, "search_docs", args).await;
        let found = &found["structuredContent"];
        assert_eq!(found["command"], "docs search");
        assert_eq!(found["data"]["docs_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(found["data"]["results"].as_array().unwrap().len(), 2);
        let options = crate::docs::Options {
            query: "transaction".to_owned(),
            json: true,
            limit: 2,
        };
        let index = crate::docs::index::packaged().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let cli = crate::docs::report(&index, &options, &cwd);
        assert_eq!(found, &serde_json::to_value(&cli).unwrap());

        let code = json!({ "code": "models.E003" });
        let explained = call_with(&mut s, 2, "explain_diagnostic", code).await;
        let entry = &explained["structuredContent"]["data"]["entries"][0];
        assert_eq!(entry["code"], "models.E003", "{explained}");
        let unknown = json!({ "code": "nope.E999" });
        let tool = "explain_diagnostic";
        let unknown = call_with(&mut s, 3, tool, unknown).await;
        let diags = &unknown["structuredContent"]["diagnostics"];
        assert_eq!(diags[0]["code"], "explain.E001", "{unknown}");

        let listed = call_with(&mut s, 4, "list_commands", json!({})).await;
        let commands = &listed["structuredContent"]["data"]["commands"];
        let names: Vec<&str> = commands
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["name"].as_str())
            .collect();
        assert!(names.contains(&"verify") && names.contains(&"mcp"));

        let empty = call_with(&mut s, 5, "search_docs", json!({})).await;
        assert_eq!(empty["isError"], true, "{empty}");
        let symbols = json!({ "query": "!!" });
        let symbols = call_with(&mut s, 6, "search_docs", symbols).await;
        assert_eq!(symbols["isError"], true, "{symbols}");
        s.close().await.unwrap();
    }
}
