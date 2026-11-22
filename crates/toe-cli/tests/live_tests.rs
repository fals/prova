//! Medium tests for levels 2 and 3: per-test events from live output,
//! first failure before the run ends, slow tests (ADR 0004, 0013).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOE: &str = env!("CARGO_BIN_EXE_toe");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("toe-it2-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn toe(dir: &Path, args: &[&str]) -> Output {
    Command::new(TOE)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run toe")
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}

fn detach(dir: &Path, args: &[&str]) -> String {
    let mut a = vec!["run", "--detach", "--json"];
    a.extend_from_slice(args);
    json(&toe(dir, &a))["run"].as_str().unwrap().to_owned()
}

#[test]
fn first_failure_arrives_while_the_suite_is_still_running() {
    let d = workspace("first-failure");
    // libtest-style output: one failure early, then the suite hangs.
    let script = "echo 'test a::ok ... ok'; echo 'test a::bad ... FAILED'; exec sleep 60";
    let id = detach(&d, &["--", "sh", "-c", script]);
    let w = toe(&d, &["wait", &id, "--max-wait", "20s", "--json"]);
    let s = json(&w);
    assert_eq!(w.status.code(), Some(1));
    assert_eq!(s["reason"], "failure");
    assert_eq!(s["status"], "running", "reported before the run ended");
    assert_eq!(s["level"], "recognized");
    assert_eq!(s["failures"][0]["test"], "a::bad");
    toe(&d, &["cancel", &id]);
    toe(&d, &["wait", &id, "--until", "done", "--max-wait", "20s"]);
}

#[test]
fn go_json_stream_reaches_native_level() {
    let d = workspace("go-json");
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../toe-adapters/fixtures/go-json.txt"
    );
    std::fs::copy(fixture, d.join("out.txt")).unwrap();
    let s = json(&toe(
        &d,
        &["run", "--json", "--", "sh", "-c", "cat out.txt; exit 1"],
    ));
    assert_eq!(s["level"], "native");
    assert_eq!(s["counts"]["passed"], 3);
    assert_eq!(s["counts"]["failed"], 1);
    assert_eq!(s["failures"][0]["message"], "x_test.go:6: want 2 got 3");
    assert!(
        s["output_tail"].as_array().is_none_or(|a| a.is_empty()),
        "messages explain the failure"
    );
}

#[test]
fn slow_test_is_reported_by_name_without_killing_it() {
    let d = workspace("slow");
    std::fs::write(
        d.join("toe.toml"),
        r#"[suite.small]
run = "echo '{\"Action\":\"run\",\"Package\":\"p\",\"Test\":\"TestHang\"}'; exec sleep 60"
size = "small"
"#,
    )
    .unwrap();
    let id = detach(&d, &["small"]);
    let w = toe(&d, &["wait", &id, "--max-wait", "20s", "--json"]);
    let s = json(&w);
    assert_eq!(s["reason"], "slow", "{s}");
    assert_eq!(w.status.code(), Some(3));
    assert_eq!(s["slow"][0]["test"], "p::TestHang");
    assert_eq!(s["slow"][0]["budget_ms"], 1000, "small-size default budget");
    assert_eq!(s["status"], "running");
    toe(&d, &["cancel", &id]);
    let end = json(&toe(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "done",
            "--max-wait",
            "20s",
            "--json",
        ],
    ));
    assert_eq!(
        end["counts"]["errored"], 1,
        "open test closed as errored at the end"
    );
}

#[test]
fn live_and_file_results_are_not_double_counted() {
    let d = workspace("both");
    let script = r#"echo 'test m::a ... ok'; echo 'test m::b ... FAILED'
mkdir -p r; cat > r/junit.xml <<XML
<testsuite name="m"><testcase classname="m" name="a"/><testcase classname="m" name="b"><failure message="boom"/></testcase><testcase classname="m" name="c"/></testsuite>
XML
exit 1"#;
    let s = json(&toe(&d, &["run", "--json", "--", "sh", "-c", script]));
    assert_eq!(s["counts"]["passed"], 2, "a live, c only in file: {s}");
    assert_eq!(s["counts"]["failed"], 1);
    assert_eq!(s["level"], "recognized");
}
