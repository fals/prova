//! Medium tests for levels 2 and 3: per-test events from live output,
//! first failure before the run ends, slow tests (ADR 0004, 0013).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-it2-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn prova(dir: &Path, args: &[&str]) -> Output {
    Command::new(PROVA)
        .args(args)
        .current_dir(dir)
        .env("PROVA_HOME", home_of(dir))
        .output()
        .expect("run prova")
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}

fn detach(dir: &Path, args: &[&str]) -> String {
    let mut a = vec!["run", "--detach", "--json"];
    a.extend_from_slice(args);
    json(&prova(dir, &a))["run"].as_str().unwrap().to_owned()
}

#[test]
fn first_failure_arrives_while_the_suite_is_still_running() {
    let d = workspace("first-failure");
    // libtest-style output: one failure early, then the suite hangs.
    let script = "echo 'test a::ok ... ok'; echo 'test a::bad ... FAILED'; exec sleep 60";
    let id = detach(&d, &["--", "sh", "-c", script]);
    let w = prova(&d, &["wait", &id, "--max-wait", "20s", "--json"]);
    let s = json(&w);
    assert_eq!(w.status.code(), Some(1));
    assert_eq!(s["reason"], "failure");
    assert_eq!(s["status"], "running", "reported before the run ended");
    assert_eq!(s["level"], "recognized");
    assert_eq!(s["failures"][0]["test"], "a::bad");
    prova(&d, &["cancel", &id]);
    prova(&d, &["wait", &id, "--until", "done", "--max-wait", "20s"]);
}

#[test]
fn go_json_stream_reaches_native_level() {
    let d = workspace("go-json");
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../prova-adapters/fixtures/go-json.txt"
    );
    std::fs::copy(fixture, d.join("out.txt")).unwrap();
    let s = json(&prova(
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
        d.join("prova.toml"),
        r#"[suite.small]
run = "echo '{\"Action\":\"run\",\"Package\":\"p\",\"Test\":\"TestHang\"}'; exec sleep 60"
size = "small"
"#,
    )
    .unwrap();
    let id = detach(&d, &["small"]);
    let w = prova(&d, &["wait", &id, "--max-wait", "20s", "--json"]);
    let s = json(&w);
    assert_eq!(s["reason"], "slow", "{s}");
    assert_eq!(w.status.code(), Some(3));
    assert_eq!(s["slow"][0]["test"], "p::TestHang");
    assert_eq!(s["slow"][0]["budget_ms"], 1000, "small-size default budget");
    assert_eq!(s["status"], "running");
    prova(&d, &["cancel", &id]);
    let end = json(&prova(
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
    let s = json(&prova(&d, &["run", "--json", "--", "sh", "-c", script]));
    assert_eq!(s["counts"]["passed"], 2, "a live, c only in file: {s}");
    assert_eq!(s["counts"]["failed"], 1);
    assert_eq!(s["level"], "recognized");
}

/// Regressions from the M5 usability session (fresh agent, rust fixture).
#[test]
fn quiet_output_says_what_toe_could_not_see() {
    let d = workspace("quiet");
    let script = "echo '.F.'; echo 'm::b --- FAILED'; echo 'test result: FAILED. 2 passed; 1 failed; 0 ignored'; exit 101";
    let s = json(&prova(&d, &["run", "--json", "--", "sh", "-c", script]));
    assert_eq!(s["counts"]["failed"], 1);
    let note = s["notes"][0].as_str().expect("note about hidden tests");
    assert!(note.contains("reported 3 tests but prova saw 1"), "{note}");
}

#[test]
fn failure_on_code_where_the_suite_passed_is_flagged_likely_flaky() {
    let d = workspace("same-code");
    std::fs::write(d.join("state"), "pass").unwrap();
    let script = "if [ \"$(cat state)\" = pass ]; then echo 'test m::a ... ok'; else echo 'test m::a ... FAILED'; exit 1; fi";
    prova(&d, &["run", "--json", "--", "sh", "-c", script]);
    std::fs::write(d.join("state"), "fail").unwrap();
    let s = json(&prova(&d, &["run", "--json", "--", "sh", "-c", script]));
    assert_eq!(
        s["failures"][0]["context"]["suite_passed_on_same_code"], true,
        "{s}"
    );
}

#[test]
fn selected_runs_do_not_count_as_suite_history() {
    let d = workspace("selected");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.s]\nrun = \"echo 'test m::a ... FAILED'; exit 1\"\nselect = \"printf 'test m::%s ... ok\\n' {filter}\"\n",
    )
    .unwrap();
    prova(&d, &["run", "s", "--select", "b"]); // passes: one selected test
    let s = json(&prova(&d, &["run", "s", "--json"]));
    assert!(
        s["history"].is_null(),
        "the selected run is not suite history: {s}"
    );
}

/// Regression (usability session 2): waiting on a finished run reports the
/// end, not "failing (run still going)".
#[test]
fn wait_on_a_finished_run_reports_done() {
    let d = workspace("finished");
    prova(
        &d,
        &[
            "run",
            "--",
            "sh",
            "-c",
            "echo 'test a ... FAILED'; echo 'test b ... FAILED'; exit 1",
        ],
    );
    let w = prova(&d, &["wait", "last", "--after", "1", "--json"]);
    let s = json(&w);
    assert_eq!(s["reason"], "done", "{s}");
    assert_eq!(s["status"], "finished");
    assert_eq!(w.status.code(), Some(1));
}

/// Isolated prova home for a test workspace, outside it (ADR 0014).
fn home_of(dir: &Path) -> PathBuf {
    PathBuf::from(format!("{}.home", dir.display()))
}
