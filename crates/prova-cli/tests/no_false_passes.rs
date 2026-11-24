//! Medium tests for ADR 0017: a pass needs evidence. Each case is a command
//! printing what a real framework prints; real processes are needed because
//! the verdict combines the exit code, the output and the store's history.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-nfp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
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

/// A little real work before the output: prova does not believe test
/// results printed by a command that used no CPU at all (round 10).
const WORK: &str = "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; ";

fn run_sh(dir: &Path, script: &str) -> (Option<i32>, serde_json::Value) {
    let script = format!("{WORK}{script}");
    let o = prova(dir, &["run", "--json", "--", "sh", "-c", &script]);
    let out = String::from_utf8_lossy(&o.stdout);
    let last = out
        .lines()
        .rev()
        .find(|l| l.starts_with('{'))
        .unwrap_or("{}");
    (o.status.code(), serde_json::from_str(last).unwrap())
}

/// (case, script, expected outcome, expected exit, text the explanation contains)
const CASES: &[(&str, &str, &str, i32, &str)] = &[
    (
        "libtest_zero",
        "echo 'running 0 tests'; echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out'",
        "failed",
        1,
        "counts 0 tests",
    ),
    (
        "go_no_test_files",
        "printf '?   \\texample.com/x\\t[no test files]\\n'",
        "failed",
        1,
        "no test ran",
    ),
    (
        "jest_no_tests",
        "echo 'No tests found, exiting with code 0'",
        "failed",
        1,
        "no test ran",
    ),
    (
        "maven_no_tests",
        "echo '[INFO] No tests to run.'; echo '[INFO] BUILD SUCCESS'",
        "failed",
        1,
        "no test ran",
    ),
    (
        "vstest_no_tests",
        "echo 'No test is available in /x.dll.'",
        "failed",
        1,
        "no test ran",
    ),
    (
        "pytest_exit_5",
        "echo 'no tests ran in 0.01s'; exit 5",
        "failed",
        1,
        "no test ran",
    ),
    (
        "unfinished_test",
        "echo '=== RUN   TestA'",
        "failed",
        1,
        "did not finish",
    ),
    (
        "summary_beats_exit",
        "echo 'test a ... ok'; echo 'test result: FAILED. 1 passed; 3 failed'",
        "failed",
        1,
        "reported 3 failed",
    ),
    (
        "not_found",
        "nosuchtestrunner",
        "errored",
        4,
        "could not start",
    ),
    (
        "jest_failed_suite_exit0",
        "printf '%s\\n' 'Test Suites: 1 failed, 1 passed, 2 total' 'Tests:       3 passed, 3 total'; exit 0",
        "failed",
        1,
        "failed test file",
    ),
    (
        "build_error",
        "echo 'error[E0308]: mismatched types'; echo 'error: could not compile'; exit 101",
        "failed",
        1,
        "first error at line 1",
    ),
];

#[test]
fn contradicted_passes_fail_and_explain_themselves() {
    for (name, script, outcome, exit, says) in CASES {
        let d = workspace(name);
        let (code, s) = run_sh(&d, script);
        assert_eq!(s["outcome"], *outcome, "{name}: {s}");
        assert_eq!(code, Some(*exit), "{name}: {s}");
        let detail = s["detail"].as_str().unwrap_or("");
        assert!(detail.contains(says), "{name}: detail {detail:?}");
        assert_eq!(s["verified"], false, "{name}");
    }
}

#[test]
fn thin_passes_are_marked_unverified() {
    for (name, script, says) in [
        ("empty", "true", "no test was observed"),
        (
            "all_skipped",
            "echo 'test a ... ignored'; echo 'test result: ok. 0 passed; 0 failed; 1 ignored'",
            "were skipped",
        ),
        (
            "crash_after_summary",
            "echo 'test a ... ok'; echo 'test result: ok. 1 passed'; echo 'thread main panicked at boom'",
            "error outside any failing test",
        ),
    ] {
        let d = workspace(name);
        let (code, s) = run_sh(&d, script);
        assert_eq!(
            (s["outcome"].as_str(), code),
            (Some("passed"), Some(0)),
            "{name}: {s}"
        );
        assert_eq!(s["verified"], false, "{name}");
        assert!(s["warnings"].to_string().contains(says), "{name}: {s}");
        let human = String::from_utf8_lossy(&prova(&d, &["status", "last"]).stdout).into_owned();
        assert!(
            human.contains("PASSED (unverified") && human.contains("warning: "),
            "{name}: {human}"
        );
    }
    let d = workspace("clean");
    let (_, s) = run_sh(
        &d,
        "echo 'test a ... ok'; echo 'test result: ok. 1 passed; 0 failed'",
    );
    assert_eq!(
        (s["outcome"].as_str(), s["verified"].as_bool()),
        (Some("passed"), Some(true))
    );
    assert!(s.get("warnings").is_none());
}

#[test]
fn tests_that_disappear_are_noticed() {
    let d = workspace("vanish");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.t]\nrun = \"sh t.sh\"\n\n[suite.lint]\nrun = \"true\"\nexpect_tests = false\n",
    )
    .unwrap();
    let run = |script: &str| {
        std::fs::write(d.join("t.sh"), format!("{WORK}{script}")).unwrap();
        let o = prova(&d, &["run", "t", "--json"]);
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        serde_json::from_str::<serde_json::Value>(
            out.lines().rev().find(|l| l.starts_with('{')).unwrap(),
        )
        .unwrap()
    };
    run("echo 'test a ... ok'; echo 'test b ... ok'; echo 'test c ... ok'");
    let some = run("echo 'test a ... ok'");
    assert_eq!(some["outcome"], "passed");
    let w = some["warnings"].to_string();
    assert!(
        w.contains("2 test(s) from the last passing run") && w.contains("b, c"),
        "{w}"
    );
    let none = run("echo 'built, no test output'");
    assert_eq!(none["outcome"], "failed", "{none}");
    assert!(none["detail"].as_str().unwrap().contains("observed none"));
    // A suite that declares it runs no tests stays a clean pass.
    let o = prova(&d, &["run", "lint", "--json"]);
    let lint: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        (lint["outcome"].as_str(), lint["verified"].as_bool()),
        (Some("passed"), Some(true))
    );
}

/// Round 10: test results from a command that did no work were believed
/// (a `--- PASS` line echoed in a Java project).
// Needs the children's CPU time, which macOS does not give (getting started).
#[cfg(target_os = "linux")]
#[test]
fn results_printed_without_any_work_are_unverified() {
    let d = workspace("nowork");
    let o = prova(
        &d,
        &[
            "run",
            "--json",
            "--",
            "sh",
            "-c",
            "echo 'test calc::divides ... ok'",
        ],
    );
    let out = String::from_utf8_lossy(&o.stdout);
    let s: serde_json::Value =
        serde_json::from_str(out.lines().rev().find(|l| l.starts_with('{')).unwrap()).unwrap();
    assert_eq!(s["verified"], false, "{s}");
    assert!(
        s["warnings"].to_string().contains("used no CPU time"),
        "{s}"
    );
}

/// Round 10: `mvn -fn` (and a crashed fork) end with exit 0 but say the
/// build failed.
#[test]
fn a_build_that_says_it_failed_fails() {
    let d = workspace("buildfail");
    let (code, s) = run_sh(
        &d,
        "echo 'test a ... ok'; echo '[INFO] BUILD FAILURE'; exit 0",
    );
    assert_eq!(
        (s["outcome"].as_str(), code),
        (Some("failed"), Some(1)),
        "{s}"
    );
    assert!(
        s["detail"]
            .as_str()
            .unwrap_or("")
            .contains("the build says it failed"),
        "{s}"
    );
}

/// Round 10: a Go module with a `cmd/` package and no tests there failed a
/// green `make test` ("[no test files]" read as "no test ran").
#[test]
fn a_package_without_tests_next_to_an_ok_one_is_not_a_failure() {
    let d = workspace("nogotests");
    let (code, s) = run_sh(
        &d,
        "printf 'ok  \\texample.com/calc\\t0.01s\\n?   \\texample.com/calc/cmd/x\\t[no test files]\\n'",
    );
    assert_eq!(
        (s["outcome"].as_str(), code),
        (Some("passed"), Some(0)),
        "{s}"
    );
    assert_eq!(s["verified"], false, "only the exit code backs it: {s}");
}

/// Round 10: stale result files refreshed with `touch`, in a run longer
/// than any test: the same results, to the millisecond, read again.
#[test]
fn results_identical_to_an_earlier_run_are_unverified() {
    let d = workspace("samefiles");
    let junit = "<testsuite name=\"s\"><testcase classname=\"s\" name=\"a\" time=\"0.130\"/><testcase classname=\"s\" name=\"b\" time=\"0.270\"/><testcase classname=\"s\" name=\"c\" time=\"0.050\"/></testsuite>";
    std::fs::write(d.join("j.xml"), junit).unwrap();
    let script =
        format!("{WORK}mkdir -p build/test-results; cp j.xml build/test-results/junit.xml");
    let (_, first) = run_sh(&d, &script);
    assert_eq!(first["verified"], true, "{first}");
    let (_, again) = run_sh(&d, &script);
    assert_eq!(again["verified"], false, "{again}");
    assert!(
        again["warnings"]
            .to_string()
            .contains("same results read again"),
        "{again}"
    );
}

/// ADR 0024: a test that passed only on a retry: the run passes (its last
/// attempt decides) but unverified, with the reason.
#[test]
fn a_retry_pass_is_an_unverified_pass() {
    let d = workspace("rerun");
    let junit = "<testsuite name=\"s\"><testcase classname=\"s\" name=\"c\" time=\"0.002\"><flakyFailure message=\"race\"/></testcase></testsuite>";
    std::fs::write(d.join("j.xml"), junit).unwrap();
    let (code, s) = run_sh(
        &d,
        "mkdir -p build/test-results; cp j.xml build/test-results/junit.xml",
    );
    assert_eq!(
        (s["outcome"].as_str(), code),
        (Some("passed"), Some(0)),
        "{s}"
    );
    assert_eq!(s["verified"], false, "{s}");
    assert!(
        s["warnings"].to_string().contains("passed on a retry"),
        "{s}"
    );
}

/// Round 11: `exit(0)` inside a test under `cargo test --test-threads=1`:
/// the binary announced 2 tests, reported 1 and stopped before its summary.
#[test]
fn a_test_binary_that_stops_before_its_summary_fails() {
    let d = workspace("stopped");
    let (code, s) = run_sh(
        &d,
        "echo 'running 2 tests'; echo 'test tests::adds ... ok'; echo 'test tests::divides ... '",
    );
    assert_eq!(
        (s["outcome"].as_str(), code),
        (Some("failed"), Some(1)),
        "{s}"
    );
    assert!(
        s["detail"]
            .as_str()
            .unwrap_or("")
            .contains("stopped after 1 of 2 tests"),
        "{s}"
    );
}

/// Round 11: saved test output replayed with a little CPU work passed as
/// verified; its own summary says it took longer than the whole run.
#[test]
fn a_replayed_summary_longer_than_the_run_is_unverified() {
    let d = workspace("replayed");
    let (_, s) = run_sh(
        &d,
        "echo 'running 1 test'; echo 'test tests::slow ... ok'; echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.50s'",
    );
    assert_eq!(s["verified"], false, "{s}");
    assert!(
        s["warnings"].to_string().contains("output was replayed"),
        "{s}"
    );
}

/// Round 11: the `exit(0)` detection crashed the supervisor (attempts are
/// immutable) and left the test recorded as passed.
#[test]
fn a_nextest_pass_whose_harness_never_reported_fails_cleanly() {
    let d = workspace("harnessdied");
    let (code, s) = run_sh(
        &d,
        "echo '        PASS [   0.004s] (1/1) calc tests::divides'; echo '  output ───'; echo '    running 1 test'; echo '     Summary [   0.004s] 1 test run: 1 passed'",
    );
    assert_eq!(
        (s["status"].as_str(), s["outcome"].as_str(), code),
        (Some("finished"), Some("failed"), Some(1)),
        "{s}"
    );
    let h = String::from_utf8_lossy(&prova(&d, &["history", "tests::divides"]).stdout).into_owned();
    assert!(
        h.contains("0 runs with a result"),
        "no evidence in history: {h}"
    );
}

fn failure_names(s: &serde_json::Value) -> Vec<String> {
    s["failures"]
        .as_array()
        .map(|f| {
            f.iter()
                .map(|t| t["test"].as_str().unwrap_or("").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Round 14: a pass on a retry within one run is flaky, whatever prints it:
/// the run follows the command's exit, with a warning and no verification.
#[test]
fn text_retries_that_pass_are_flaky() {
    for (name, script) in [
        (
            "nextest_retry",
            "printf '%s\\n' '        PASS [   0.004s] calc tests::adds' '  TRY 1 FAIL [   0.004s] calc tests::retry_me' '  RETRY 2/2 [         ] calc tests::retry_me' '  TRY 2 PASS [   0.003s] calc tests::retry_me' '     Summary [   0.020s] 2 tests run: 2 passed (1 flaky), 0 skipped' '   FLAKY 2/2 [   0.003s] calc tests::retry_me'; exit 0",
        ),
        (
            "pytest_rerun",
            "printf '%s\\n' 'tests/test_a.py::test_ok PASSED [ 50%]' 'tests/test_a.py::test_flaky RERUN [100%]' 'tests/test_a.py::test_flaky PASSED [100%]' '==== 2 passed, 1 rerun in 0.10s ===='; exit 0",
        ),
    ] {
        let d = workspace(name);
        let (code, s) = run_sh(&d, script);
        assert_eq!(
            (s["outcome"].as_str(), code),
            (Some("passed"), Some(0)),
            "{name}: {s}"
        );
        assert_eq!(s["verified"], false, "{name}: {s}");
        assert!(
            s["warnings"].to_string().contains("passed on a retry"),
            "{name}: {s}"
        );
        assert_eq!(s["counts"]["failed"], 0, "{name}: {s}");
    }
}

/// Round 14: a pytest teardown error on a passed test is that one test
/// erroring; and a collection error is named.
#[test]
fn pytest_errors_outside_the_call_are_named() {
    let d = workspace("pytest_teardown");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' 'tests/test_b.py::test_ok PASSED [ 50%]' 'tests/test_b.py::test_ok ERROR [ 50%]' 'tests/test_b.py::test_next PASSED [100%]' '==== 2 passed, 1 error in 0.10s ===='; exit 1",
    );
    assert_eq!(
        (s["outcome"].as_str(), code),
        (Some("failed"), Some(1)),
        "{s}"
    );
    assert_eq!(failure_names(&s), vec!["tests.test_b::test_ok"], "{s}");
    assert_eq!(s["counts"]["errored"], 1, "{s}");
    assert_eq!(s["counts"]["passed"], 1, "{s}");

    let d = workspace("pytest_collect");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' '==== short test summary info ====' 'ERROR tests/test_e.py - ImportError: no module x' '!!!! Interrupted: 1 error during collection !!!!'; exit 2",
    );
    assert_eq!(
        (s["outcome"].as_str(), code),
        (Some("failed"), Some(1)),
        "{s}"
    );
    assert_eq!(failure_names(&s), vec!["tests/test_e.py::(file)"], "{s}");
}

/// Round 14: Jest's verbose reporter names its tests and failures.
#[test]
fn jest_verbose_output_is_recognized() {
    let d = workspace("jest_verbose");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' 'PASS src/a.test.ts' '  calc' '    \u{2713} adds (2 ms)' '    \u{2713} subtracts' ' FAIL  src/x.test.ts' '  calc' '    \u{2715} flaky clock (3 ms)' '    \u{2713} divides' '' '  \u{25cf} calc \u{203a} flaky clock' '' '    expect(received).toBe(expected)' 'Test Suites: 1 failed, 1 passed, 2 total' 'Tests:       1 failed, 3 passed, 4 total'; exit 1",
    );
    assert_eq!(
        (s["outcome"].as_str(), code),
        (Some("failed"), Some(1)),
        "{s}"
    );
    assert_eq!(
        failure_names(&s),
        vec!["src/x.test.ts::calc > flaky clock"],
        "{s}"
    );
    assert_eq!(s["counts"]["passed"], 3, "{s}");
}

/// Round 14: a Jest file that fails to load is one errored entry.
#[test]
fn jest_file_that_fails_to_load_is_an_entry() {
    let d = workspace("jest_file");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' 'PASS src/a.test.ts' 'FAIL src/b.test.ts' '  \u{25cf} Test suite failed to run' '' \"    Cannot find module './missing'\" '' 'Test Suites: 1 failed, 1 passed, 2 total' 'Tests:       3 passed, 3 total'; exit 0",
    );
    assert_eq!(
        (s["outcome"].as_str(), code),
        (Some("failed"), Some(1)),
        "{s}"
    );
    assert_eq!(failure_names(&s), vec!["src/b.test.ts::(file)"], "{s}");
}

/// Round 14: a JUnit header that counts an error no test case shows (Gradle
/// `ignoreFailures`, exit 0) is a failed run.
#[test]
fn a_junit_header_error_without_a_case_fails() {
    let d = workspace("junit_header");
    let junit = "<testsuite name=\"com.acme.CalcTest\" tests=\"3\" errors=\"1\" failures=\"0\"><testcase classname=\"com.acme.CalcTest\" name=\"adds\" time=\"0.1\"/><testcase classname=\"com.acme.CalcTest\" name=\"subs\" time=\"0.1\"/><system-err>ExceptionInInitializerError</system-err></testsuite>";
    std::fs::write(d.join("j.xml"), junit).unwrap();
    let (code, s) = run_sh(
        &d,
        "mkdir -p build/test-results/test; cp j.xml build/test-results/test/TEST-com.acme.CalcTest.xml",
    );
    assert_eq!(
        (s["outcome"].as_str(), code),
        (Some("failed"), Some(1)),
        "{s}"
    );
    assert_eq!(failure_names(&s), vec!["com.acme.CalcTest::(suite)"], "{s}");
}

/// Round 14: Maven's console summary names a failing class the (partial)
/// JUnit files do not.
#[test]
fn surefire_console_failures_are_recorded() {
    let d = workspace("surefire_console");
    let junit = "<testsuite name=\"CalcTest\" tests=\"1\"><testcase classname=\"com.acme.CalcTest\" name=\"adds\" time=\"0.1\"/></testsuite>";
    std::fs::write(d.join("j.xml"), junit).unwrap();
    let (code, s) = run_sh(
        &d,
        "mkdir -p target/surefire-reports; cp j.xml target/surefire-reports/TEST-com.acme.CalcTest.xml; printf '%s\\n' '[ERROR] Failures: ' '[ERROR]   OtherTest.fooWorks:12 expected:<2> but was:<3>' '[ERROR] Tests run: 2, Failures: 1, Errors: 0, Skipped: 0'; exit 0",
    );
    assert_eq!(
        (s["outcome"].as_str(), code),
        (Some("failed"), Some(1)),
        "{s}"
    );
    assert_eq!(failure_names(&s), vec!["OtherTest::fooWorks"], "{s}");
}

/// Round 14: Vitest's `FAIL file [ file ]` (exit 0 from the wrapper) is a
/// failing entry named by its file.
#[test]
fn a_vitest_file_that_failed_to_load_fails_the_run() {
    let d = workspace("vitest_file");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' ' \u{2713} src/a.test.ts (3 tests) 5ms' ' FAIL  src/b.test.ts [ src/b.test.ts ]' 'Error: Cannot find module ./missing' ' Test Files  1 failed | 1 passed (2)' '      Tests  3 passed (3)'; exit 0",
    );
    assert_eq!(
        (s["outcome"].as_str(), code),
        (Some("failed"), Some(1)),
        "{s}"
    );
    assert_eq!(failure_names(&s), vec!["src/b.test.ts::(file)"], "{s}");
}

/// Round 15: Surefire's console names a failure `CalcTest.flaky`, its
/// report `calc.CalcTest`: the failure was recorded under the short name and
/// passes under the long one, two tests whose history never met (a flaky
/// test "failed every time", a regression "first time seen"). A
/// parameterized case's console name lacks its arguments and index.
#[test]
fn surefire_console_failures_take_the_reports_name() {
    let d = workspace("surefire_identity");
    let junit = "<testsuite name=\"calc.CalcTest\" tests=\"3\" failures=\"2\"><testcase classname=\"calc.CalcTest\" name=\"adds\" time=\"0.1\"/><testcase classname=\"calc.CalcTest\" name=\"flaky\" time=\"0.1\"><failure message=\"tick\"/></testcase><testcase classname=\"calc.CalcTest\" name=\"many(int, int)[2]\" time=\"0.1\"><failure message=\"bad\"/></testcase></testsuite>";
    std::fs::write(d.join("j.xml"), junit).unwrap();
    let (code, s) = run_sh(
        &d,
        "mkdir -p target/surefire-reports; cp j.xml target/surefire-reports/TEST-calc.CalcTest.xml; printf '%s\\n' '[ERROR] Failures: ' '[ERROR]   CalcTest.flaky:23 tick' '[ERROR]   CalcTest.many:30 bad' '[ERROR] Tests run: 3, Failures: 2, Errors: 0, Skipped: 0'; exit 1",
    );
    assert_eq!(code, Some(1), "{s}");
    assert_eq!(
        failure_names(&s),
        vec!["calc.CalcTest::flaky", "calc.CalcTest::many(int, int)[2]"],
        "{s}"
    );
    assert_eq!(s["counts"]["failed"], 2, "{s}");
    assert_eq!(s["counts"]["passed"], 1, "{s}");
}

/// Round 16: every Vitest unhandled error was `vitest::(unhandled errors)`,
/// so a new leaked rejection was "[failed 1x before]" for an unrelated
/// earlier error, with no error text.
#[test]
fn a_vitest_unhandled_error_is_named_by_its_origin_and_error() {
    let d = workspace("vitest_unhandled_origin");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' ' \u{2713} src/extra.test.ts (1 test) 2ms' '' '\u{23af}\u{23af}\u{23af}\u{23af} Unhandled Errors \u{23af}\u{23af}\u{23af}\u{23af}' '' 'Vitest caught 1 unhandled error during the test run.' 'This might cause false positive tests. Resolve unhandled errors to make sure your tests are not affected.' '' '\u{23af}\u{23af} Unhandled Rejection \u{23af}\u{23af}' 'Error: boom from a leaked promise' ' \u{276f} src/extra.test.ts:3:18' 'This error originated in \"src/extra.test.ts\" test file. It doesn'\"'\"'t mean the error was thrown inside the file itself, but while it was running.' '\u{23af}\u{23af}\u{23af}\u{23af}\u{23af}\u{23af}\u{23af}\u{23af}' '' ' Test Files  1 passed (1)' '      Tests  1 passed (1)' '     Errors  1 error'; exit 1",
    );
    assert_eq!(code, Some(1), "{s}");
    assert_eq!(
        failure_names(&s),
        vec!["src/extra.test.ts::(unhandled error)"],
        "{s}"
    );
    assert_eq!(
        s["failures"][0]["message"], "Error: boom from a leaked promise",
        "{s}"
    );
}

/// Round 16: older pytest-subtests prints `test_other [case 2] SUBFAIL`;
/// the failure was attributed to no test ("no test failed; exit 1").
#[test]
fn an_older_pytest_subtests_failure_names_its_test() {
    let d = workspace("subfail_old");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' 'tests/test_q.py::test_ok PASSED' 'tests/test_q.py::test_other [case 1] SUBPASS' 'tests/test_q.py::test_other [case 2] SUBFAIL' 'tests/test_q.py::test_other PASSED' '========= 3 passed, 1 subtests failed in 0.05s ========='; exit 1",
    );
    assert_eq!(code, Some(1), "{s}");
    let names = failure_names(&s);
    assert!(
        names
            .iter()
            .any(|n| n.contains("test_other") && n.contains("case 2")),
        "{s}"
    );
}
