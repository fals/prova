//! Medium tests for round 17 (counts, history, wording): real runs are
//! needed because history and labels come from the store.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-r17-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(".gitignore"), "state\n").unwrap();
    let vcs = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&d)
            .output()
            .unwrap()
    };
    vcs(&["init", "-q"]);
    vcs(&["add", "-A"]);
    vcs(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "init",
    ]);
    d
}

fn prova(dir: &Path, args: &[&str]) -> Output {
    Command::new(PROVA)
        .args(args)
        .current_dir(dir)
        .env("PROVA_HOME", format!("{}.home", dir.display()))
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

/// A little real work first: prova does not believe results printed by a
/// command that used no CPU (round 10).
const WORK: &str = "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; ";

fn suite(d: &Path, body: &str) {
    std::fs::write(d.join("prova.toml"), body).unwrap();
}

/// Why medium: history reads runs the store recorded.
#[test]
fn history_suite_option_limits_the_runs() {
    let d = workspace("suite");
    let ok = format!("{WORK}echo 'test calc::x ... ok'");
    let bad = format!("{WORK}echo 'test calc::x ... FAILED'; exit 1");
    suite(
        &d,
        &format!("[suite.make]\nrun = {bad:?}\n\n[suite.default]\nrun = {ok:?}\n"),
    );
    prova(&d, &["run", "make"]);
    prova(&d, &["run", "default"]);
    // One entry per suite (ADR 0037), each with its own runs.
    let all = text(&prova(&d, &["history", "calc::x"]));
    assert!(
        all.contains("(suite default)") && all.contains("(suite make)"),
        "{all}"
    );
    assert!(
        all.contains("1 runs with a result, 0 failed")
            && all.contains("1 runs with a result, 1 failed"),
        "{all}"
    );
    let one = text(&prova(&d, &["history", "calc::x", "--suite", "default"]));
    assert!(
        one.contains("1 runs with a result, 0 failed") && !one.contains("(suite make)"),
        "{one}"
    );
}

#[test]
fn history_with_an_empty_name_lists_every_test() {
    let d = workspace("empty");
    let cmd = format!("{WORK}echo 'test calc::a ... ok'; echo 'test calc::b ... ok'");
    suite(&d, &format!("[suite.default]\nrun = {cmd:?}\n"));
    prova(&d, &["run", "default"]);
    let o = prova(&d, &["history", ""]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(out.contains("calc::a") && out.contains("calc::b"), "{out}");
    assert!(!out.contains("be more specific"), "{out}");
}

/// A pytest verbose line has no message and no time; the summary line names
/// the cause. History shows the cause, and says the time is unknown.
#[test]
fn history_keeps_the_failure_message_and_does_not_invent_a_duration() {
    let d = workspace("msg");
    let cmd = format!(
        "{WORK}echo 'tests/t.py::test_div FAILED  [ 50%]'; echo 'tests/t.py::test_ok PASSED  [100%]'; echo 'FAILED tests/t.py::test_div - assert 1 == 2'; echo '=== 1 failed, 1 passed in 0.1s ==='; exit 1"
    );
    suite(&d, &format!("[suite.default]\nrun = {cmd:?}\n"));
    prova(&d, &["run", "default"]);
    let out = text(&prova(&d, &["history", "test_div"]));
    assert!(out.contains("assert 1 == 2"), "{out}");
    let ok = text(&prova(&d, &["history", "test_ok"]));
    assert!(!ok.contains("p50 0ms"), "{ok}");
}

/// `make test` running `cargo test` stops at the first failing crate: the
/// framework's count fell below the last passing run's, and prova says so.
#[test]
fn a_fall_in_the_frameworks_count_names_the_tests_that_did_not_run() {
    let d = workspace("fewer");
    let run = format!(
        "{WORK}if [ \"$(cat state)\" = pass ]; then echo 'test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s'; else echo 'test result: FAILED. 4 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s'; exit 1; fi"
    );
    suite(&d, &format!("[suite.default]\nrun = {run:?}\n"));
    std::fs::write(d.join("state"), "pass").unwrap();
    prova(&d, &["run", "default"]);
    std::fs::write(d.join("state"), "fail").unwrap();
    let out = text(&prova(&d, &["run", "default"]));
    assert!(
        out.contains("counted 5 test(s), the last passing run") && out.contains("4 did not run"),
        "{out}"
    );
}

/// Wording: one subtest is "1 subtest", and a failing parent is listed
/// before its failing subtest whichever the runner printed first.
#[test]
fn one_subtest_is_singular_and_the_parent_comes_first() {
    let d = workspace("subtest");
    let run = "printf '=== RUN   TestP\\n=== RUN   TestP/a\\n    p_test.go:9: boom\\n    --- FAIL: TestP/a (0.00s)\\n--- FAIL: TestP (0.00s)\\nFAIL\\nFAIL\\texample.com/p\\t0.01s\\n'; exit 1";
    suite(
        &d,
        &format!("[suite.default]\nrun = {:?}\n", format!("{WORK}{run}")),
    );
    let out = text(&prova(&d, &["run", "default"]));
    assert!(out.contains("(and 1 subtest, 1 failed)"), "{out}");
    let parent = out.find("FAIL example.com/p::TestP ").unwrap_or(usize::MAX);
    let child = out
        .find("FAIL example.com/p::TestP/a")
        .unwrap_or(usize::MAX);
    assert!(parent < child, "{out}");
}

/// Every run of a `--repeat` that fails prints its live FAIL line, however
/// fast it ended (round 17: run 8 of 8 printed none).
#[test]
fn every_repeat_prints_its_live_fail_line() {
    let d = workspace("repeatlive");
    let run = format!("{WORK}echo 'test calc::x ... FAILED'; exit 1");
    suite(&d, &format!("[suite.default]\nrun = {run:?}\n"));
    let o = prova(&d, &["run", "default", "--repeat", "4"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(err.matches("FAIL calc::x").count(), 4, "{err}");
}

/// Go `-count=4` (pass, fail, pass, fail) in one run: four attempts, and a
/// test that both passed and failed is flaky evidence (ADR 0029 section 2).
#[test]
fn go_count_repeats_that_passed_and_failed_are_flaky_with_all_attempts() {
    let d = workspace("gocount");
    let run = "printf '=== RUN   TestC\\n--- PASS: TestC (0.00s)\\n=== RUN   TestC\\n    c_test.go:9: lost\\n--- FAIL: TestC (0.00s)\\n=== RUN   TestC\\n--- PASS: TestC (0.00s)\\n=== RUN   TestC\\n    c_test.go:9: lost\\n--- FAIL: TestC (0.00s)\\nFAIL\\nFAIL\\texample.com/c\\t0.01s\\n'; exit 1";
    suite(
        &d,
        &format!("[suite.default]\nrun = {:?}\n", format!("{WORK}{run}")),
    );
    let out = text(&prova(&d, &["run", "default"]));
    assert!(out.contains("FLAKY"), "{out}");
    let h = text(&prova(&d, &["history", "TestC"]));
    assert!(h.contains("(4 attempts)"), "{h}");
}

/// The other order (fail, then pass in a later iteration of the same run)
/// is the same evidence: the run fails, the test is flaky, and the pass is
/// not lost (round 17: it was labelled a plain failure).
#[test]
fn go_count_repeats_that_failed_then_passed_are_flaky_too() {
    let d = workspace("gocountfp");
    let run = "printf '=== RUN   TestC\\n    c_test.go:9: lost\\n--- FAIL: TestC (0.00s)\\n=== RUN   TestC\\n--- PASS: TestC (0.00s)\\nFAIL\\nFAIL\\texample.com/c\\t0.01s\\n'; exit 1";
    suite(
        &d,
        &format!("[suite.default]\nrun = {:?}\n", format!("{WORK}{run}")),
    );
    let o = prova(&d, &["run", "default"]);
    let out = text(&o);
    assert_eq!(
        o.status.code(),
        Some(1),
        "a failure in any iteration fails: {out}"
    );
    assert!(out.contains("FLAKY"), "{out}");
    let h = text(&prova(&d, &["history", "TestC"]));
    assert!(h.contains("(2 attempts)"), "{h}");
    assert!(
        h.contains("both passed and failed on 1 code version"),
        "{h}"
    );
}
