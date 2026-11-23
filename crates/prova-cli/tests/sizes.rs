//! Medium: test sizes come from the processes and sockets a test used,
//! never from its duration (ADR 0025). Needs real processes, a real socket
//! on localhost and the real binary, so it cannot be smaller.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-sizes-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn prova(dir: &Path, port: u16, args: &[&str]) -> Output {
    Command::new(PROVA)
        .args(args)
        .current_dir(dir)
        .env("PROVA_HOME", dir.with_extension("home"))
        .env("PORT", port.to_string())
        .output()
        .expect("run prova")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// A framework in libtest's JSON format with three tests, each longer than
/// two of prova's samples: one that only computes (the longest), one that
/// runs a child process, one that holds a socket to localhost. The waits
/// are in the shell itself (no process), and work in bash 3.2 (macOS).
/// Each test spins first: what a test starts in the instant its start line
/// is read can be missed (a lower bound, ADR 0025 §5).
const SUITE: &str = r#"
spin() { end=$((SECONDS + $1)); while [ $SECONDS -lt $end ]; do :; done; }
echo '{"type":"suite","event":"started","test_count":3}'
echo '{"type":"test","event":"started","name":"m::computes"}'
spin 3
echo '{"type":"test","name":"m::computes","event":"ok"}'
echo '{"type":"test","event":"started","name":"m::runs_a_child"}'
spin 1
sleep 1.5
echo '{"type":"test","name":"m::runs_a_child","event":"ok"}'
echo '{"type":"test","event":"started","name":"m::uses_localhost"}'
spin 1
exec 3<>/dev/tcp/127.0.0.1/$PORT
spin 2
exec 3>&-
echo '{"type":"test","name":"m::uses_localhost","event":"ok"}'
echo '{"type":"suite","event":"ok","passed":3,"failed":0}'
"#;

fn sizes_of(d: &Path, port: u16, run: &str) -> Vec<(String, String, String)> {
    let o = prova(d, port, &["events", run]);
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|e| e["kind"] == "test_size")
        .map(|e| {
            (
                e["test"]["name"].as_str().unwrap_or_default().to_owned(),
                e["size"].as_str().unwrap_or_default().to_owned(),
                e["evidence"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

#[test]
fn sizes_follow_processes_and_sockets_not_time() {
    let d = workspace("resources");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::fs::write(d.join("t.sh"), SUITE).unwrap();
    std::fs::write(
        d.join("prova.toml"),
        "[suite.t]\nrun = \"bash t.sh\"\nsize = \"small\"\n",
    )
    .unwrap();
    let run = || {
        let o = prova(&d, port, &["run", "t", "--json"]);
        let v: serde_json::Value =
            serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&o)));
        v["run"].as_str().unwrap().to_owned()
    };
    let first = run();
    // One attributed run is not enough to call a test small (ADR 0025 §5).
    let h = text(&prova(&d, port, &["history", "m::computes"]));
    assert!(h.contains("size: small? (1 of 3 runs)"), "{h}");

    let sizes = sizes_of(&d, port, &first);
    let get = |n: &str| {
        sizes
            .iter()
            .find(|(t, _, _)| t.ends_with(n))
            .unwrap_or_else(|| panic!("no size for {n}: {sizes:?}"))
            .clone()
    };
    // The longest test is small: it used one process and no socket.
    assert_eq!(get("computes").1, "small", "{sizes:?}");
    let child = get("runs_a_child");
    assert_eq!(child.1, "medium", "{sizes:?}");
    assert!(child.2.contains("sleep"), "{child:?}");
    let net = get("uses_localhost");
    assert_eq!(net.1, "medium", "{sizes:?}");
    assert!(net.2.contains("local socket"), "{net:?}");

    // Declared small, seen using more: said, naming the resource.
    let notes = text(&prova(&d, port, &["status", &first]));
    assert!(
        notes.contains("runs_a_child is declared small but started sleep"),
        "{notes}"
    );

    run();
    run();
    let h = text(&prova(&d, port, &["history", "m::computes"]));
    assert!(
        h.contains("size: small (one process, no sockets, in 3 runs)"),
        "{h}"
    );

    // `prova sizes`: by resources, with the reason.
    let report = text(&prova(&d, port, &["sizes"]));
    assert!(
        report.contains("small 1") && report.contains("medium 2"),
        "{report}"
    );
    assert!(report.contains("started sleep"), "{report}");
    drop(listener);
}
