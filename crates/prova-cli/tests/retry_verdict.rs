//! A test that failed and then passed on a retry passes the run (ADR 0024,
//! `spec/run.qnt` retryPassTest): checked end to end with the output of
//! real runners, through the whole engine and the store. Medium: the verdict
//! combines the exit code, the recognizers and the store, which no unit
//! test covers together.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-retry-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Runs a command that prints `body` and exits with `code`; returns the
/// exit code and the run's JSON summary.
fn run_printing(d: &Path, body: &str, code: i32) -> (Option<i32>, serde_json::Value) {
    // A little real work first: prova does not believe test results printed
    // by a command that used no CPU at all (round 10).
    let script = format!(
        "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; cat <<'PROVA_EOF'\n{body}\nPROVA_EOF\nexit {code}"
    );
    let o = Command::new(PROVA)
        .args(["run", "--json", "--", "sh", "-c", &script])
        .current_dir(d)
        .env("PROVA_HOME", format!("{}.home", d.display()))
        .output()
        .expect("run prova");
    let out = String::from_utf8_lossy(&o.stdout);
    let last = out
        .lines()
        .rev()
        .find(|l| l.starts_with('{'))
        .unwrap_or("{}");
    (o.status.code(), serde_json::from_str(last).unwrap())
}

fn passed_after_retry(code: Option<i32>, s: &serde_json::Value) {
    assert_eq!(code, Some(0), "{s}");
    assert_eq!(s["outcome"], "passed", "{s}");
    assert_eq!(s["counts"]["failed"], 0, "{s}");
    assert!(
        s.to_string().contains("failed, then passed on a retry"),
        "flaky is said: {s}"
    );
}

/// Real cargo-nextest 0.9.152 `--retries 1` with the libtest JSON stream
/// (the `#2` attempt in the JSON, `TRY 1 FAIL` and `TRY 2 PASS` lines).
#[test]
fn a_nextest_libtest_json_retry_that_passes_passes_the_run() {
    let d = workspace("nextest-json");
    let body = std::fs::read_to_string(fixtures().join("retry-nextest-libtest-json.txt")).unwrap();
    // The capture ends with the exit code nextest returned.
    assert!(body.trim_end().ends_with("exit 0"), "{body}");
    let body = body.trim_end().trim_end_matches("exit 0");
    let (code, s) = run_printing(&d, body, 0);
    passed_after_retry(code, &s);
}

/// nextest's human output of a retry (the quarantine corpus' real capture).
#[test]
fn a_nextest_text_retry_that_passes_passes_the_run() {
    let d = workspace("nextest-text");
    let body =
        std::fs::read_to_string(fixtures().join("quarantine/cargo-nextest-retry.txt")).unwrap();
    let (code, s) = run_printing(&d, &body, 0);
    passed_after_retry(code, &s);
}

/// pytest-rerunfailures, verbose and quiet: `RERUN` or `R`, then a pass.
#[test]
fn a_pytest_rerun_that_passes_passes_the_run() {
    let verbose = "============================= test session starts ==============================\n\
collected 2 items\n\n\
tests/test_a.py::test_ok PASSED                                          [ 50%]\n\
tests/test_a.py::test_flaky RERUN                                        [100%]\n\
tests/test_a.py::test_flaky PASSED                                       [100%]\n\n\
========================= 2 passed, 1 rerun in 0.10s ==========================";
    let d = workspace("pytest-v");
    let (code, s) = run_printing(&d, verbose, 0);
    passed_after_retry(code, &s);

    let quiet = ".R.                                                                      [100%]\n\
2 passed, 1 rerun in 0.10s";
    let d = workspace("pytest-q");
    let (code, s) = run_printing(&d, quiet, 0);
    assert_eq!(code, Some(0), "{s}");
    assert_eq!(s["counts"]["failed"], 0, "{s}");
}

/// The other side of the rule: a retry that fails again fails the run.
#[test]
fn a_nextest_retry_that_fails_again_fails_the_run() {
    let d = workspace("nextest-fail");
    let body = "    Starting 1 test across 1 binary\n\
  TRY 1 FAIL [   0.003s] (───) calc tests::retried\n\
  stdout ───\n\n    running 1 test\n    test tests::retried ... FAILED\n\n    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n\
  TRY 2 FAIL [   0.003s] (1/1) calc tests::retried\n\
  stdout ───\n\n    running 1 test\n    test tests::retried ... FAILED\n\n    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n\
────────────\n     Summary [   0.008s] 1 test run: 0 passed, 1 failed, 0 skipped\n        FAIL [   0.003s] (1/1) calc tests::retried";
    let (code, s) = run_printing(&d, body, 100);
    assert_eq!(code, Some(1), "{s}");
    assert_eq!(s["outcome"], "failed", "{s}");
}
