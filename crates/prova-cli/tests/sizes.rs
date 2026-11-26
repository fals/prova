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
/// is read can be missed (a lower bound, ADR 0025 §5). The suite spins
/// before its first test too: the shell's own start, in the same instant
/// as an immediate first test, was once that test's (CI run 45642558379).
/// `spin N` waits N-1 to N seconds (`$SECONDS` counts whole seconds), so
/// `spin 2` keeps at least 1 s, and a sample, before the first test. The socket is held
/// for 3 to 4 s: `$SECONDS` counts whole seconds, and sampling sockets on
/// macOS is slow (a 1 to 2 s hold was missed there). The child sleeps 3 s
/// for the same reason: macOS samples processes with `ps`, and a 1.5 s child
/// was missed on a slow shared runner (main CI), so the test came out small.
const SUITE: &str = r#"
spin() { end=$((SECONDS + $1)); while [ $SECONDS -lt $end ]; do :; done; }
spin 2
echo '{"type":"suite","event":"started","test_count":3}'
echo '{"type":"test","event":"started","name":"m::computes"}'
spin 3
echo '{"type":"test","name":"m::computes","event":"ok"}'
echo '{"type":"test","event":"started","name":"m::runs_a_child"}'
spin 1
sleep 3
echo '{"type":"test","name":"m::runs_a_child","event":"ok"}'
echo '{"type":"test","event":"started","name":"m::uses_localhost"}'
spin 1
exec 3<>/dev/tcp/127.0.0.1/$PORT
spin 4
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
    assert!(
        h.contains("size: small? (nothing more seen in 1 run(s) that could show it; 3 needed)"),
        "{h}"
    );

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
        h.contains("size: small (no other process or socket seen, in 3 runs)"),
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

/// Runs the suite `n` times; returns the last run id.
fn runs(d: &Path, port: u16, n: usize) -> String {
    let mut last = String::new();
    for _ in 0..n {
        let o = prova(d, port, &["run", "t", "--json"]);
        let v: serde_json::Value =
            serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&o)));
        last = v["run"].as_str().unwrap().to_owned();
    }
    last
}

fn suite(d: &Path, body: &str, declared: Option<&str>) {
    let lib = r#"S() { echo "{\"type\":\"test\",\"event\":\"started\",\"name\":\"$1\"}"; }
OK() { echo "{\"type\":\"test\",\"name\":\"$1\",\"event\":\"ok\"}"; }
spin() { end=$((SECONDS + $1)); while [ $SECONDS -lt $end ]; do :; done; }
"#;
    std::fs::write(d.join("t.sh"), format!("{lib}{body}")).unwrap();
    let size = declared.map_or(String::new(), |s| format!("size = \"{s}\"\n"));
    std::fs::write(
        d.join("prova.toml"),
        format!("[suite.t]\nrun = \"bash t.sh\"\n{size}"),
    )
    .unwrap();
}

/// Round 12: a test that starts a child the moment it begins had it in the
/// snapshot taken when its start line was read, so it looked small. Start
/// times decide (ADR 0026 §1).
#[cfg(target_os = "linux")]
#[test]
fn a_child_started_at_once_is_the_tests() {
    let d = workspace("at-once");
    // As runners do: something printed before the test starts.
    suite(
        &d,
        "echo running 1 test\nspin 2\nS m::kid\nsleep 1.6\nOK m::kid\n",
        None,
    );
    let run = runs(&d, 0, 1);
    let sizes = sizes_of(&d, 0, &run);
    assert!(
        sizes
            .iter()
            .any(|(t, s, e)| t.ends_with("kid") && s == "medium" && e.contains("sleep")),
        "{sizes:?}"
    );
}

/// Round 12: output that reaches prova all at once (a buffered pipe) gave
/// "small" with no sample inside the test. Nothing sampled, nothing
/// claimed (ADR 0026 §3, §4).
#[test]
fn output_that_comes_at_once_claims_no_size() {
    let d = workspace("buffered");
    suite(
        &d,
        "{ S m::net; sleep 1.2; OK m::net; S m::b; OK m::b; } > buf.txt\ncat buf.txt\n",
        None,
    );
    let run = runs(&d, 0, 3);
    assert!(
        sizes_of(&d, 0, &run).is_empty(),
        "{:?}",
        sizes_of(&d, 0, &run)
    );
    let h = text(&prova(&d, 0, &["history", "m::net"]));
    assert!(!h.contains("size: small"), "{h}");
}

/// Tests prova cannot isolate are held to their run's test phase: a run
/// that used a socket bounds them and names it, and a suite declared small
/// is told (ADR 0026 §2, §5).
#[test]
fn overlapping_tests_are_bounded_by_what_their_run_used() {
    let d = workspace("overlap");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    suite(
        &d,
        // Waits in whole seconds (bash 3.2): the socket is held at least
        // 2 s, so the phase is sampled several times.
        "S m::a\nS m::b\nspin 2\nexec 3<>/dev/tcp/127.0.0.1/$PORT\nspin 3\nexec 3>&-\nOK m::a\nOK m::b\n",
        Some("small"),
    );
    let run = runs(&d, port, 1);
    let status = text(&prova(&d, port, &["status", &run]));
    assert!(
        status.contains("this suite is declared small but its run 1 local socket(s)"),
        "{status}"
    );
    let h = text(&prova(&d, port, &["history", "m::a"]));
    assert!(
        h.contains("size: medium or less (never seen on its own, and its run 1 local socket(s))"),
        "{h}"
    );
    drop(listener);
}

/// A test too fast to be sampled on its own is small when its whole run
/// showed nothing beyond the runner, after 3 runs (ADR 0026 §2).
#[test]
fn fast_tests_in_a_clean_run_become_small() {
    let d = workspace("clean");
    suite(
        &d,
        "S m::fast1\nOK m::fast1\nspin 3\nS m::fast2\nOK m::fast2\n",
        None,
    );
    runs(&d, 0, 3);
    let h = text(&prova(&d, 0, &["history", "m::fast1"]));
    assert!(
        h.contains("size: small (no other process or socket seen, in 3 runs)"),
        "{h}"
    );
}
