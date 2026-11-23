//! `prova mcp`: thin MCP facade over the CLI (ADR 0009). JSON-RPC 2.0, one
//! message per line on stdio. Each tool runs this same binary with
//! `--json`, so MCP and CLI cannot drift apart. Nothing depends on
//! progress notifications, subscriptions or tasks: every call returns
//! within `MAX_WAIT_S` and carries the handle (run id) for the next call.

use std::io::{BufRead, Write};
use std::process::Command;

use serde_json::{Value, json};

/// Below the 60 s default tool timeout of common clients.
const MAX_WAIT_S: u64 = 45;
const VERSIONS: &[&str] = &["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26"];

fn tools() -> Value {
    let summary = json!({"type": "object", "description": "Run summary: reason, status, outcome, level, counts, failures (with history context), slow tests, last_seq, next-step hints"});
    json!([
        {
            "name": "run_start",
            "description": "Start a test run in the background and return its run id at once. Give a suite from prova.toml, or a command as an argument array (no shell). Then call run_wait.",
            "inputSchema": {"type": "object", "properties": {
                "suite": {"type": "string", "description": "Suite name from prova.toml"},
                "select": {"type": "string", "description": "Run only tests matching this name (suite's select template)"},
                "command": {"type": "array", "items": {"type": "string"}, "description": "Command and arguments, e.g. [\"make\", \"test\"]"}
            }},
            "outputSchema": {"type": "object", "properties": {"run": {"type": "string"}}, "required": ["run"]}
        },
        {
            "name": "run_wait",
            "description": "Wait for the next significant event of a run: first failure, slow test, stall, or end. Returns within max_wait_s (<= 45) with reason 'running' if nothing happened; call again with after = last_seq.",
            "inputSchema": {"type": "object", "required": ["run"], "properties": {
                "run": {"type": "string", "description": "Run id, prefix, or 'last'"},
                "until": {"type": "string", "enum": ["first-failure", "stall", "done"]},
                "after": {"type": "integer", "minimum": 0},
                "max_wait_s": {"type": "integer", "minimum": 1, "maximum": MAX_WAIT_S}
            }},
            "outputSchema": summary
        },
        {
            "name": "run_status",
            "description": "Summary of one run (with failures and history), or the recent runs when run is omitted.",
            "inputSchema": {"type": "object", "properties": {"run": {"type": "string"}}}
        },
        {
            "name": "run_cancel",
            "description": "Cancel a run: SIGTERM to its process group, then SIGKILL after a grace period.",
            "inputSchema": {"type": "object", "required": ["run"], "properties": {"run": {"type": "string"}}}
        },
        {
            "name": "test_history",
            "description": "History and flake statistics of a test: failure rate, flips on the same code, passed-on-retry, durations, recent outcomes.",
            "inputSchema": {"type": "object", "required": ["test"], "properties": {
                "test": {"type": "string", "description": "Substring of the test's full name"}
            }}
        }
    ])
}

fn str_arg<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str)
}

/// Maps a tool call to CLI arguments.
pub fn cli_args(name: &str, args: &Value) -> Result<Vec<String>, String> {
    let mut a: Vec<String> = Vec::new();
    let push = |a: &mut Vec<String>, s: &str| a.push(s.to_owned());
    match name {
        "run_start" => {
            push(&mut a, "run");
            push(&mut a, "--detach");
            push(&mut a, "--json");
            let command: Vec<String> = match args.get("command") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(v)) => v
                    .iter()
                    .map(|x| {
                        x.as_str()
                            .map(str::to_owned)
                            .ok_or("command items must be strings")
                    })
                    .collect::<Result<_, _>>()?,
                Some(_) => return Err("command must be an array of strings".into()),
            };
            match (str_arg(args, "suite"), command.is_empty()) {
                (Some(_), false) => return Err("give suite or command, not both".into()),
                (Some(s), true) => push(&mut a, s),
                (None, _) => {}
            }
            if let Some(s) = str_arg(args, "select") {
                a.push(format!("--select={s}"));
            }
            if !command.is_empty() {
                push(&mut a, "--");
                a.extend(command);
            }
        }
        "run_wait" => {
            let run = str_arg(args, "run").ok_or("run is required")?;
            let max = args
                .get("max_wait_s")
                .and_then(Value::as_u64)
                .unwrap_or(30)
                .clamp(1, MAX_WAIT_S);
            a.extend([
                "wait".into(),
                run.into(),
                "--json".into(),
                "--max-wait".into(),
                format!("{max}s"),
            ]);
            if let Some(u) = str_arg(args, "until") {
                a.push(format!("--until={u}"));
            }
            if let Some(n) = args.get("after").and_then(Value::as_u64) {
                a.push(format!("--after={n}"));
            }
        }
        "run_status" => {
            push(&mut a, "status");
            push(&mut a, "--json");
            if let Some(r) = str_arg(args, "run") {
                push(&mut a, r);
            }
        }
        "run_cancel" => {
            push(&mut a, "cancel");
            push(&mut a, str_arg(args, "run").ok_or("run is required")?);
        }
        "test_history" => {
            push(&mut a, "history");
            push(&mut a, "--json");
            push(&mut a, str_arg(args, "test").ok_or("test is required")?);
        }
        _ => return Err(format!("unknown tool {name}")),
    }
    Ok(a)
}

fn call_tool(name: &str, args: &Value) -> Value {
    let argv = match cli_args(name, args) {
        Ok(a) => a,
        Err(e) => return json!({"isError": true, "content": [{"type": "text", "text": e}]}),
    };
    let out = std::env::current_exe().and_then(|exe| Command::new(exe).args(&argv).output());
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            return json!({"isError": true, "content": [{"type": "text", "text": format!("prova failed to start: {e}")}]});
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    let code = out.status.code().unwrap_or(-1);
    // Exit codes 0, 1, 3, 5 are results; 2 and 4 are errors (ADR 0004).
    let is_error = matches!(code, 2 | 4) || code < 0;
    let text = if stdout.is_empty() {
        stderr.clone()
    } else {
        stdout.clone()
    };
    let mut result = json!({
        "isError": is_error,
        "content": [{"type": "text", "text": format!("{text}\n(exit {code})")}],
    });
    if let Ok(v) = serde_json::from_str::<Value>(&stdout)
        && v.is_object()
    {
        result["structuredContent"] = v;
    }
    result
}

fn handle(msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone(); // notifications get no response
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let info = json!({"name": "prova", "version": env!("CARGO_PKG_VERSION")});
    let instructions = "Run tests with run_start then run_wait (short, repeat with after=last_seq). React to the first failure while the run continues; use run_cancel to stop. Never wait on a long tool call.";
    let result = match method {
        "initialize" => {
            let asked = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(VERSIONS[0]);
            let version = if VERSIONS.contains(&asked) {
                asked
            } else {
                VERSIONS[0]
            };
            json!({"protocolVersion": version, "capabilities": {"tools": {}}, "serverInfo": info, "instructions": instructions})
        }
        "server/discover" => json!({
            "supportedVersions": VERSIONS, "capabilities": {"tools": {}}, "serverInfo": info, "instructions": instructions
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            call_tool(name, &args)
        }
        _ => {
            return Some(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {method}")}}),
            );
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

pub fn serve() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg),
            Err(e) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}),
            ),
        };
        if let Some(r) = reply {
            writeln!(stdout, "{r}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_arguments_map_to_cli_without_a_shell() {
        let a = cli_args("run_start", &json!({"command": ["make", "test; rm -rf /"]})).unwrap();
        assert_eq!(
            a,
            vec!["run", "--detach", "--json", "--", "make", "test; rm -rf /"]
        );
        let a = cli_args("run_start", &json!({"suite": "small", "select": "-rf"})).unwrap();
        assert_eq!(
            a,
            vec!["run", "--detach", "--json", "small", "--select=-rf"]
        );
        assert!(cli_args("run_start", &json!({"suite": "s", "command": ["x"]})).is_err());
        let w = cli_args(
            "run_wait",
            &json!({"run": "last", "max_wait_s": 600, "after": 4}),
        )
        .unwrap();
        assert!(w.contains(&"45s".to_string()), "max wait capped: {w:?}");
        assert!(w.contains(&"--after=4".to_string()));
        assert!(cli_args("nope", &json!({})).is_err());
    }

    #[test]
    fn protocol_basics() {
        let r = handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}})).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert!(
            handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none()
        );
        let t = handle(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
        assert_eq!(t["result"]["tools"].as_array().unwrap().len(), 5);
        let e = handle(&json!({"jsonrpc": "2.0", "id": 3, "method": "bogus"})).unwrap();
        assert_eq!(e["error"]["code"], -32601);
    }
}
