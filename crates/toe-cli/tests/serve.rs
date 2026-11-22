//! Medium tests for `toe serve` (ADR 0014 section 3): replication from a
//! client, reads from another machine, offline spool, authorization.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

const TOE: &str = env!("CARGO_BIN_EXE_toe");
const TOKEN: &str = "test-token-0123456789";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("toe-serve-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Server {
    child: Child,
    url: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn server(home: &Path, tokens: &str, port: u16) -> Server {
    let mut child = Command::new(TOE)
        .args([
            "serve",
            "--listen",
            &format!("127.0.0.1:{port}"),
            "--threads",
            "8",
        ])
        .env("TOE_HOME", home)
        .env("TOE_SERVE_TOKENS", tokens)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addr = line
        .split("listening on ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .to_owned();
    Server {
        child,
        url: format!("http://{addr}"),
    }
}

fn toe(ws: &Path, home: &Path, url: &str, token: &str, args: &[&str]) -> Output {
    Command::new(TOE)
        .args(args)
        .current_dir(ws)
        .env("TOE_HOME", home)
        .env("TOE_SERVER", url)
        .env("TOE_TOKEN", token)
        .output()
        .unwrap()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

/// A git repository; `clone` gives a second checkout with the same identity.
fn repo(base: &Path, name: &str) -> PathBuf {
    let ws = base.join(name);
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("f"), name).unwrap();
    for args in [
        &["init", "-q"][..],
        &["add", "-A"],
        &["commit", "-qm", "init"],
    ] {
        let ok = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(&ws)
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }
    ws
}

fn clone(from: &Path, to: &Path) {
    assert!(
        Command::new("git")
            .args(["clone", "-q"])
            .arg(from)
            .arg(to)
            .status()
            .unwrap()
            .success()
    );
}

const FAILING: &[&str] = &[
    "run",
    "--json",
    "--",
    "sh",
    "-c",
    "echo 'test m::a ... ok'; echo 'test m::b ... FAILED'; exit 1",
];

#[test]
fn runs_in_a_pod_are_visible_from_another_machine() {
    let base = dir("replicate");
    let srv = server(&base.join("server-home"), TOKEN, 0);
    let pod = repo(&base, "pod");
    let laptop = base.join("laptop");
    clone(&pod, &laptop);

    let ran = json(&toe(&pod, &base.join("pod-home"), &srv.url, TOKEN, FAILING));
    let run = ran["run"].as_str().unwrap();

    // The laptop has no local copy of the run: everything comes from the server.
    let lap_home = base.join("laptop-home");
    let w = toe(
        &laptop,
        &lap_home,
        &srv.url,
        TOKEN,
        &[
            "wait",
            run,
            "--until",
            "done",
            "--max-wait",
            "10s",
            "--json",
        ],
    );
    let s = json(&w);
    assert_eq!(w.status.code(), Some(1));
    assert_eq!(s["outcome"], "failed");
    assert_eq!(s["failures"][0]["test"], "m::b");
    assert_eq!(s["counts"]["passed"], 1);
    let out = toe(&laptop, &lap_home, &srv.url, TOKEN, &["output", run]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("test m::b ... FAILED"),
        "output uploaded"
    );
    let h = json(&toe(
        &laptop,
        &lap_home,
        &srv.url,
        TOKEN,
        &["history", "m::b", "--json"],
    ));
    assert_eq!(h["failures"], 1);
}

#[test]
fn offline_runs_are_kept_and_synced_later() {
    let base = dir("offline");
    let ws = repo(&base, "ws");
    let home = base.join("home");
    // Nothing listens on port 9 (discard): the run must still complete.
    let o = toe(&ws, &home, "http://127.0.0.1:9", TOKEN, FAILING);
    assert_eq!(
        o.status.code(),
        Some(1),
        "local result unaffected by the dead server"
    );
    let run = json(&o)["run"].as_str().unwrap().to_owned();

    let srv = server(&base.join("server-home"), TOKEN, 0);
    let synced = json(&toe(&ws, &home, &srv.url, TOKEN, &["sync", "--json"]));
    assert_eq!(synced["synced"][0], run.as_str());
    let other = base.join("other");
    clone(&ws, &other);
    let s = json(&toe(
        &other,
        &base.join("other-home"),
        &srv.url,
        TOKEN,
        &["status", &run, "--json"],
    ));
    assert_eq!(s["status"], "finished");
    assert_eq!(s["failures"][0]["test"], "m::b");
    // Syncing again sends nothing and changes nothing.
    let again = json(&toe(&ws, &home, &srv.url, TOKEN, &["sync", "--json"]));
    assert_eq!(again["synced"].as_array().unwrap().len(), 0);
}

#[test]
fn tokens_are_required_and_scoped() {
    let base = dir("auth");
    let ws = repo(&base, "ws");
    let scoped = "scoped-token-0123456789 0000000000000000";
    let srv = server(&base.join("server-home"), &format!("{TOKEN};{scoped}"), 0);
    let home = base.join("home");
    let o = toe(&ws, &home, &srv.url, TOKEN, FAILING);
    let run = json(&o)["run"].as_str().unwrap().to_owned();
    let other = base.join("other");
    clone(&ws, &other);
    let oh = base.join("other-home");

    let bad = toe(
        &other,
        &oh,
        &srv.url,
        "wrong-token-0123456789",
        &["status", &run],
    );
    assert_eq!(bad.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("401"));
    let denied = toe(
        &other,
        &oh,
        &srv.url,
        "scoped-token-0123456789",
        &["status", &run],
    );
    assert_eq!(denied.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&denied.stderr).contains("403"));

    let refuse = Command::new(TOE)
        .args(["serve", "--listen", "127.0.0.1:0"])
        .env("TOE_HOME", base.join("h2"))
        .env_remove("TOE_SERVE_TOKENS")
        .output()
        .unwrap();
    assert_eq!(refuse.status.code(), Some(4), "no tokens, no server");
}
