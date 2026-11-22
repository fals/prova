//! Medium test: the MCP facade over stdio, end to end (ADR 0009).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

#[test]
fn mcp_client_can_start_and_wait_for_a_run() {
    let d = std::env::temp_dir().join(format!("toe-mcp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_toe"))
        .arg("mcp")
        .current_dir(&d)
        .env("TOE_HOME", home_of(&d))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut call = |id: u64, method: &str, params: Value| -> Value {
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(stdin, "{msg}").unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let init = call(
        1,
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "toe");
    let started = call(
        2,
        "tools/call",
        json!({"name": "run_start", "arguments": {"command": ["sh", "-c", "echo 'test a ... FAILED'; exit 1"]}}),
    );
    let run = started["result"]["structuredContent"]["run"]
        .as_str()
        .expect("run id")
        .to_owned();
    let waited = call(
        3,
        "tools/call",
        json!({"name": "run_wait", "arguments": {"run": run, "until": "done", "max_wait_s": 20}}),
    );
    let s = &waited["result"]["structuredContent"];
    assert_eq!(s["outcome"], "failed", "{waited}");
    assert_eq!(s["failures"][0]["test"], "a");
    assert_eq!(
        waited["result"]["isError"], false,
        "a failing test is a result, not a tool error"
    );
    let bad = call(
        4,
        "tools/call",
        json!({"name": "run_wait", "arguments": {"run": "zzzz"}}),
    );
    assert_eq!(bad["result"]["isError"], true);
    drop(stdin);
    child.wait().unwrap();
}

/// Isolated toe home for a test workspace, outside it (ADR 0014).
fn home_of(dir: &Path) -> PathBuf {
    PathBuf::from(format!("{}.home", dir.display()))
}
