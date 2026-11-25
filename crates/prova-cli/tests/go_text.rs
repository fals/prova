//! Medium tests for `go test` and gotestsum text output (round 14): the
//! package is part of a test's identity, repeated executions are attempts,
//! reruns that pass are flaky, and a failed package is never covered.
//! Real processes: the verdict combines the exit code, the output and the
//! store's history.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

/// A script named `go` that prints what the scripted command printed; the
/// command is `./rc/go test`.
fn fake_go(d: &Path, script: &str) {
    std::fs::create_dir_all(d.join("rc")).unwrap();
    std::fs::write(d.join("rc/go"), format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(d.join("rc/go"), std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-gotext-{name}-{}", std::process::id()));
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

fn failing(s: &serde_json::Value) -> Vec<String> {
    s["failures"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|f| f["test"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// The same test name in two packages: a failure in one is not hidden by
/// the other, and each keeps the identity the JSON stream gives it.
#[test]
fn gotestsum_testname_keeps_the_package_of_a_same_named_test() {
    let d = workspace("dup");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' 'FAIL example.com/m/a.TestDup (0.00s)' '    a_test.go:7: REAL BUG' 'FAIL example.com/m/a' \
         'PASS example.com/m/b.TestDup (0.00s)' 'ok   example.com/m/b' '' '=== Failed' \
         '=== FAIL: example.com/m/a TestDup (0.00s)' '    a_test.go:7: REAL BUG' '' 'DONE 2 tests, 1 failure in 0.2s'; exit 1",
    );
    assert_eq!(code, Some(1), "{s}");
    assert_eq!(failing(&s), vec!["example.com/m/a::TestDup"], "{s}");
    assert_eq!(s["counts"]["passed"], 1, "{s}");
}

#[test]
fn go_verbose_same_test_in_two_packages_has_no_phantom() {
    let d = workspace("dup-v");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' '=== RUN   TestA' '--- PASS: TestA (0.00s)' 'PASS' 'ok  \t example.com/m/a\t0.01s' \
         '=== RUN   TestA' '    b_test.go:3: bug' '--- FAIL: TestA (0.00s)' 'FAIL' 'FAIL\texample.com/m/b\t0.01s'; exit 1",
    );
    assert_eq!(code, Some(1), "{s}");
    assert_eq!(failing(&s), vec!["example.com/m/b::TestA"], "{s}");
    assert_eq!(s["counts"]["passed"], 1, "{s}");
    assert!(!s.to_string().contains("second result"), "{s}");
}

/// `go test -count=3`: pass, fail, pass of one test in one package run is a
/// failure: iterations are not retries (round 15).
#[test]
fn go_count_iterations_keep_the_failure() {
    let d = workspace("count");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' '=== RUN   TestRace' '--- PASS: TestRace (0.00s)' '=== RUN   TestRace' \
         '    race_test.go:9: lost update' '--- FAIL: TestRace (0.00s)' '=== RUN   TestRace' \
         '--- PASS: TestRace (0.00s)' 'FAIL' 'FAIL\texample.com/m/r\t0.01s' 'FAIL'; exit 1",
    );
    assert_eq!(code, Some(1), "the command failed: {s}");
    assert_eq!(s["outcome"], "failed", "{s}");
    assert_eq!(failing(&s), vec!["example.com/m/r::TestRace"], "{s}");
    assert!(!s.to_string().contains("second result"), "{s}");
}

/// The same with `go test -count=2 -json`: fail, then pass, exit 1.
#[test]
fn go_json_count_iterations_keep_the_failure() {
    let d = workspace("count-json");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' \
         '{\"Action\":\"run\",\"Package\":\"p\",\"Test\":\"TestF\"}' \
         '{\"Action\":\"output\",\"Package\":\"p\",\"Test\":\"TestF\",\"Output\":\"    f_test.go:4: boom\\n\"}' \
         '{\"Action\":\"fail\",\"Package\":\"p\",\"Test\":\"TestF\",\"Elapsed\":0}' \
         '{\"Action\":\"run\",\"Package\":\"p\",\"Test\":\"TestF\"}' \
         '{\"Action\":\"pass\",\"Package\":\"p\",\"Test\":\"TestF\",\"Elapsed\":0}' \
         '{\"Action\":\"fail\",\"Package\":\"p\",\"Elapsed\":0}'; exit 1",
    );
    assert_eq!(code, Some(1), "{s}");
    assert_eq!(s["outcome"], "failed", "{s}");
    assert_eq!(failing(&s), vec!["p::TestF"], "{s}");
}

/// A quarantined test failing in one iteration next to another test that
/// failed then passed: the second fails the run, named.
#[test]
fn go_count_failure_next_to_a_quarantined_one_fails_the_run() {
    let d = workspace("count-quar");
    let script = format!(
        "{WORK}printf '%s\\n' '=== RUN   TestQ' '--- PASS: TestQ (0.00s)' '=== RUN   TestQueue' \
         '    q_test.go:3: no' '--- FAIL: TestQueue (0.00s)' '=== RUN   TestQ' '    q_test.go:9: x' \
         '--- FAIL: TestQ (0.00s)' '=== RUN   TestQueue' '--- PASS: TestQueue (0.00s)' 'FAIL' \
         'FAIL\texample.com/q\t0.01s'; exit 1"
    );
    // History for TestQ, then the quarantine.
    let first = format!(
        "{WORK}printf '%s\\n' '=== RUN   TestQ' '--- FAIL: TestQ (0.00s)' 'FAIL' 'FAIL\texample.com/q\t0.01s'; exit 1"
    );
    prova(&d, &["run", "--", "sh", "-c", &first]);
    let o = prova(
        &d,
        &[
            "quarantine",
            "add",
            "example.com/q::TestQ",
            "--reason",
            "known",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let o = prova(&d, &["run", "--json", "--", "sh", "-c", &script]);
    let s: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(o.status.code(), Some(1), "{s}");
    assert_eq!(s["outcome"], "failed", "{s}");
    // Both listed; only TestQ is the known, quarantined one (ADR 0034).
    let known: Vec<&str> = s["failures"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["quarantined"] != true)
        .filter_map(|f| f["test"].as_str())
        .collect();
    assert_eq!(known, vec!["example.com/q::TestQueue"], "{s}");
}

/// Round 15: a panic in the package's own output, outside any test, next to
/// a quarantined failing test must not pass.
#[test]
fn a_go_panic_outside_a_test_fails_next_to_a_quarantined_failure() {
    let d = workspace("panic-quar");
    let first = format!(
        "{WORK}printf '%s\\n' '=== RUN   TestQ' '--- FAIL: TestQ (0.00s)' 'FAIL' 'FAIL\texample.com/q\t0.01s'; exit 1"
    );
    prova(&d, &["run", "--", "sh", "-c", &first]);
    prova(
        &d,
        &[
            "quarantine",
            "add",
            "example.com/q::TestQ",
            "--reason",
            "known",
        ],
    );
    // go test -v text
    let text_run = format!(
        "{WORK}printf '%s\\n' '=== RUN   TestQ' '    q_test.go:5: no' '--- FAIL: TestQ (0.00s)' \
         '=== RUN   TestQueue' '--- PASS: TestQueue (0.00s)' 'panic: send on closed channel' '' \
         'goroutine 12 [running]:' 'FAIL\texample.com/q\t0.01s'; exit 2"
    );
    let o = prova(&d, &["run", "--json", "--", "sh", "-c", &text_run]);
    let s: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(s["outcome"], "failed", "{s}");
    assert!(
        failing(&s).contains(&"example.com/q::(package)".to_owned()),
        "{s}"
    );
    // go test -json
    let json_run = format!(
        "{WORK}printf '%s\\n' \
         '{{\"Action\":\"run\",\"Package\":\"example.com/q\",\"Test\":\"TestQ\"}}' \
         '{{\"Action\":\"fail\",\"Package\":\"example.com/q\",\"Test\":\"TestQ\",\"Elapsed\":0}}' \
         '{{\"Action\":\"run\",\"Package\":\"example.com/q\",\"Test\":\"TestQueue\"}}' \
         '{{\"Action\":\"pass\",\"Package\":\"example.com/q\",\"Test\":\"TestQueue\",\"Elapsed\":0}}' \
         '{{\"Action\":\"output\",\"Package\":\"example.com/q\",\"Output\":\"panic: send on closed channel\\n\"}}' \
         '{{\"Action\":\"output\",\"Package\":\"example.com/q\",\"Output\":\"goroutine 12 [running]:\\n\"}}' \
         '{{\"Action\":\"fail\",\"Package\":\"example.com/q\"}}'; exit 2"
    );
    let o = prova(&d, &["run", "--json", "--", "sh", "-c", &json_run]);
    let s: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(s["outcome"], "failed", "{s}");
    assert!(
        failing(&s).contains(&"example.com/q::(package)".to_owned()),
        "{s}"
    );
}

/// Round 15: a package line printed by a running test is its output.
#[test]
fn a_package_line_forged_by_a_test_does_not_rekey_earlier_tests() {
    let d = workspace("forged");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' '=== RUN   TestA' '    a_test.go:3: no' '--- FAIL: TestA (0.00s)' '=== RUN   TestSub' \
         'ok  \texample.com/fake\t0.010s' '--- PASS: TestSub (0.00s)' 'FAIL' 'FAIL\texample.com/real\t0.02s'; exit 1",
    );
    assert_eq!(code, Some(1), "{s}");
    assert_eq!(failing(&s), vec!["example.com/real::TestA"], "{s}");
    assert_eq!(s["counts"]["passed"], 1, "{s}");
    assert_eq!(s["counts"]["errored"], 0, "{s}");
}

/// Round 15: `go test` without -v prints the message after the FAIL line;
/// a panic in a test follows it too.
#[test]
fn go_failure_messages_that_follow_the_fail_line_are_shown() {
    let d = workspace("tailmsg");
    let o = prova(
        &d,
        &[
            "run",
            "--",
            "sh",
            "-c",
            &format!(
                "{WORK}printf '%s\\n' '--- FAIL: TestX (0.00s)' '    x_test.go:9: boom' 'FAIL' 'FAIL\texample.com/x\t0.01s'; exit 1"
            ),
        ],
    );
    assert!(
        text(&o).contains("TestX [first time seen: no history yet]: x_test.go:9: boom"),
        "{}",
        text(&o)
    );
    let o = prova(
        &d,
        &[
            "run",
            "--",
            "sh",
            "-c",
            &format!(
                "{WORK}printf '%s\\n' '=== RUN   TestP' '--- FAIL: TestP (0.00s)' 'panic: boom [recovered]' '' 'goroutine 7 [running]:' \
                 '\tp_test.go:4 +0x25' 'FAIL\texample.com/x\t0.01s'; exit 2"
            ),
        ],
    );
    let out = text(&o);
    assert!(out.contains("panic: boom"), "{out}");
    assert!(!out.contains("example.com/x::(package)"), "{out}");
}

/// Round 15: the live report of a Go failure seen under its provisional
/// name (the package line comes later) knows the test failed before.
#[test]
fn a_live_go_failure_under_its_provisional_name_has_its_history() {
    let d = workspace("provisional");
    // The same command twice: one suite, the second run comes after the first.
    let script = format!(
        "{WORK}echo '=== RUN   TestFF'; echo '--- FAIL: TestFF (0.00s)'; sleep 2; echo 'FAIL\texample.com/ff\t2s'; exit 1"
    );
    let o = prova(&d, &["run", "--", "sh", "-c", &script]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let o = prova(
        &d,
        &["run", "--until", "first-failure", "--", "sh", "-c", &script],
    );
    let out = text(&o);
    assert!(out.contains("TestFF"), "{out}");
    assert!(!out.contains("first time seen"), "{out}");
}

/// gotestsum `--rerun-fails`: the rerun passed, the command exited 0: flaky.
#[test]
fn a_gotestsum_rerun_that_passes_is_flaky_not_a_failure() {
    let d = workspace("rerun");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' 'FAIL example.com/m/a.TestFlaky (0.00s)' '    a_test.go:7: flake' \
         'PASS example.com/m/a.TestFlaky (re-run 1)' 'ok   example.com/m/a' '' 'DONE 2 tests, 1 failure in 0.2s'; exit 0",
    );
    assert_eq!(code, Some(0), "{s}");
    assert_eq!(s["outcome"], "passed", "{s}");
    assert_eq!(s["verified"], false, "{s}");
    assert!(
        s.to_string().contains("failed, then passed on a retry"),
        "{s}"
    );
    let h = text(&prova(&d, &["history", "TestFlaky"]));
    assert!(h.contains("2 attempts"), "{h}");
}

/// gotestsum's own event stream: a rerun appends a second run of the test.
#[test]
fn a_gotestsum_json_rerun_that_passes_is_flaky() {
    let d = workspace("rerun-json");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' \
         '{\"Action\":\"run\",\"Package\":\"p\",\"Test\":\"TestF\"}' \
         '{\"Action\":\"output\",\"Package\":\"p\",\"Test\":\"TestF\",\"Output\":\"    f_test.go:4: flake\\n\"}' \
         '{\"Action\":\"fail\",\"Package\":\"p\",\"Test\":\"TestF\",\"Elapsed\":0}' \
         '{\"Action\":\"fail\",\"Package\":\"p\",\"Elapsed\":0}' \
         '{\"Action\":\"run\",\"Package\":\"p\",\"Test\":\"TestF\"}' \
         '{\"Action\":\"pass\",\"Package\":\"p\",\"Test\":\"TestF\",\"Elapsed\":0}' \
         '{\"Action\":\"pass\",\"Package\":\"p\",\"Elapsed\":0}'; exit 0",
    );
    assert_eq!(code, Some(0), "{s}");
    assert_eq!(s["outcome"], "passed", "{s}");
    assert_eq!(s["verified"], false, "{s}");
    assert!(
        s.to_string().contains("failed, then passed on a retry"),
        "{s}"
    );
}

/// Makefile recipes that end in `|| true` or a coverage step: the package
/// failure is printed, the exit code is 0.
#[test]
fn a_failed_go_package_fails_the_run_whatever_the_exit_code() {
    let d = workspace("pkgfail");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' '=== RUN   TestA1' '--- PASS: TestA1 (0.00s)' 'PASS' 'ok  \texample.com/m/a\t0.002s' \
         '# example.com/m/b [example.com/m/b.test]' './b_test.go:5:2: undefined: foo' \
         'FAIL\texample.com/m/b [build failed]' 'FAIL'; echo coverage written; exit 0",
    );
    assert_eq!(code, Some(1), "{s}");
    assert_eq!(s["outcome"], "failed", "{s}");
    assert_eq!(failing(&s), vec!["example.com/m/b::(package)"], "{s}");
    // TestMain / goleak: only passes, then the package fails.
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' '=== RUN   TestA' '--- PASS: TestA (0.00s)' 'PASS' 'goleak: found goroutines' \
         'FAIL\texample.com/m/c\t0.01s'; exit 0",
    );
    assert_eq!(code, Some(1), "{s}");
    assert_eq!(failing(&s), vec!["example.com/m/c::(package)"], "{s}");
}

#[test]
fn gotestsum_errors_fail_the_run_whatever_the_exit_code() {
    let d = workspace("gts-errors");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' 'PASS example.com/m/a.TestA1 (0.00s)' 'ok   example.com/m/a' \
         'FAIL example.com/m/b [build failed]' '' '=== Errors' 'b/b_test.go:5:2: undefined: foo' '' \
         'DONE 1 tests, 1 error in 0.4s'; exit 0",
    );
    assert_eq!(code, Some(1), "{s}");
    assert_eq!(failing(&s), vec!["example.com/m/b::(package)"], "{s}");
}

/// A quarantined failure plus a package that did not build must not pass.
#[test]
fn a_quarantine_never_covers_a_package_failure() {
    let d = workspace("quarantine");
    let first = format!(
        "{WORK}printf '%s\\n' 'FAIL example.com/m/a.TestX (0.00s)' '    a_test.go:3: no' 'PASS example.com/m/a.TestOk (0.00s)' \
         'DONE 2 tests, 1 failure in 0.1s'; exit 1"
    );
    let o = prova(&d, &["run", "--", "sh", "-c", &first]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    let o = prova(
        &d,
        &[
            "quarantine",
            "add",
            "example.com/m/a::TestX",
            "--reason",
            "known",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let second = format!(
        "{WORK}printf '%s\\n' 'FAIL example.com/m/a.TestX (0.00s)' '    a_test.go:3: no' 'PASS example.com/m/a.TestOk (0.00s)' \
         'FAIL\texample.com/m/b [build failed]' 'DONE 2 tests, 1 failure in 0.1s'; exit 1"
    );
    let o = prova(&d, &["run", "--", "sh", "-c", &second]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("FAILED") && out.contains("example.com/m/b::(package)"),
        "{out}"
    );
}

/// A `-run` typo matches nothing and `go test` still exits 0.
#[test]
fn go_no_tests_to_run_fails_like_pytest_no_tests_ran() {
    let d = workspace("notests");
    let o = prova(
        &d,
        &[
            "run",
            "--json",
            "--",
            "sh",
            "-c",
            "printf '%s\\n' 'testing: warning: no tests to run' 'PASS' 'ok  \texample.com/m/p\t0.001s [no tests to run]'; exit 0",
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("no test ran"), "{}", text(&o));
}

/// A `[no test files]` package next to a real one is normal.
#[test]
fn go_packages_without_tests_next_to_real_ones_are_fine() {
    let d = workspace("mixed");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' '=== RUN   TestA' '--- PASS: TestA (0.00s)' 'PASS' 'ok  \texample.com/m/a\t0.01s' \
         '?   \texample.com/m/cmd\t[no test files]'; exit 0",
    );
    assert_eq!(code, Some(0), "{s}");
    assert_eq!(s["outcome"], "passed", "{s}");
}

/// The count says 4 tests and the format prints every test: 2 are missing.
#[test]
fn a_count_above_the_printed_tests_is_unverified() {
    let d = workspace("missing");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' 'PASS example.com/m/a.TestA (0.00s)' 'PASS example.com/m/a.TestB (0.00s)' \
         'DONE 4 tests in 0.2s'; exit 0",
    );
    assert_eq!(code, Some(0), "{s}");
    assert_eq!(s["verified"], false, "{s}");
    assert!(s.to_string().contains("showed only 2"), "{s}");
}

/// `panic: test timed out` names the test that was running.
#[test]
fn a_go_timeout_names_the_hung_test() {
    let d = workspace("hang");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' '=== RUN   TestHang' 'panic: test timed out after 1m0s' '\trunning tests:' '\t\tTestHang (1m0s)' '' \
         'goroutine 33 [running]:' 'FAIL\texample.com/m/h\t60.01s' 'FAIL'; exit 1",
    );
    assert_eq!(code, Some(1), "{s}");
    assert_eq!(failing(&s), vec!["example.com/m/h::TestHang"], "{s}");
    assert_eq!(s["counts"]["errored"], 1, "{s}");
}

/// Round 14: a name recorded bare by an older prova (no package line in the
/// output) and the qualified one a later run records are one test: no false
/// "known flaky test did not run".
#[test]
fn a_bare_flaky_name_is_not_left_out_when_its_qualified_form_ran() {
    let d = workspace("bare");
    // History is per code version: a git repository gives it one.
    std::fs::write(d.join(".gitignore"), "cnt\n").unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&d)
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-qm",
        "init",
    ]);
    let script = format!(
        "{WORK}n=$(cat cnt 2>/dev/null || echo 0); n=$((n+1)); echo $n > cnt; \\
         if [ $((n % 2)) = 0 ] && [ $n -lt 6 ]; then r=FAIL; else r=PASS; fi; \\
         if [ $n -ge 6 ]; then printf '%s\\n' \"$r example.com/calc.TestFlakyClock (0.00s)\" 'DONE 1 tests in 0.1s'; \\
         else printf '%s\\n' '=== RUN   TestFlakyClock' \"--- $r: TestFlakyClock (0.00s)\"; fi; [ $r = PASS ]"
    );
    for _ in 0..5 {
        prova(&d, &["run", "--", "sh", "-c", &script]);
    }
    let o = prova(&d, &["run", "--", "sh", "-c", &script]);
    let out = text(&o);
    assert!(out.contains("PASSED"), "{out}");
    assert!(!out.contains("did not run"), "{out}");
}

/// Round 15: with gotestsum's event stream injected, its text output still
/// fed the `go test -v` recognizer: the `=== RUN` lines gotestsum echoes
/// for a failing test became unqualified tests that never finished
/// (UNFINISHED, a second identity, and a quarantine that never applied).
#[test]
fn gotestsum_text_is_ignored_when_its_stream_is_read() {
    let d = workspace("gts-stream-text");
    let json = [
        r#"{"Action":"run","Package":"example.com/calc","Test":"TestAdd"}"#,
        r#"{"Action":"pass","Package":"example.com/calc","Test":"TestAdd","Elapsed":0}"#,
        r#"{"Action":"run","Package":"example.com/calc","Test":"TestDiv"}"#,
        r#"{"Action":"output","Package":"example.com/calc","Test":"TestDiv","Output":"    calc_test.go:17: Div(8, 2) = 16, want 4\n"}"#,
        r#"{"Action":"fail","Package":"example.com/calc","Test":"TestDiv","Elapsed":0}"#,
        r#"{"Action":"fail","Package":"example.com/calc","Elapsed":0}"#,
    ];
    let mut script = String::from(WORK);
    for l in json {
        script.push_str(&format!(
            "printf '%s\\n' '{l}' >> \"$GOTESTSUM_JSONFILE\"; "
        ));
    }
    script.push_str(
        "printf '%s\\n' 'PASS example.com/calc.TestAdd (0.00s)' '=== RUN   TestDiv' \
         '    calc_test.go:17: Div(8, 2) = 16, want 4' '--- FAIL: TestDiv (0.00s)' \
         'FAIL example.com/calc.TestDiv (0.00s)' 'FAIL example.com/calc' '' '=== Failed' \
         '=== FAIL: example.com/calc TestDiv (0.00s)' '=== RUN   TestDiv' \
         '    calc_test.go:17: Div(8, 2) = 16, want 4' '--- FAIL: TestDiv (0.00s)' '' \
         'DONE 2 tests, 1 failure in 0.1s'; exit 1",
    );
    let o = prova(
        &d,
        &[
            "run",
            "--json",
            "--inject",
            "gotestsum-json",
            "--",
            "sh",
            "-c",
            &script,
        ],
    );
    let s: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(failing(&s), vec!["example.com/calc::TestDiv"], "{s}");
    assert_eq!(s["counts"]["errored"], 0, "{s}");
    assert_eq!(s["counts"]["passed"], 1, "{s}");
}

/// Round 15: `go test -v` results re-keyed under their package still
/// counted under the provisional name: a quarantine of the qualified test
/// never applied, and `--repeat` tallied each failure twice.
#[test]
fn go_verbose_failures_count_once_under_their_package() {
    let d = workspace("gov-once");
    let script = format!(
        "{WORK}printf '%s\\n' '=== RUN   TestOK' '--- PASS: TestOK (0.00s)' '=== RUN   TestF' \
         '    f_test.go:4: flake' '--- FAIL: TestF (0.00s)' 'FAIL' 'FAIL\texample.com/m\t0.01s' 'FAIL'; exit 1"
    );
    fake_go(&d, &script);
    let o = prova(&d, &["run", "--json", "--", "./rc/go", "test"]);
    let s: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(failing(&s), vec!["example.com/m::TestF"], "{s}");
    let o = prova(
        &d,
        &[
            "quarantine",
            "add",
            "example.com/m::TestF",
            "--reason",
            "flake",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = prova(&d, &["run", "--repeat", "2", "--", "./rc/go", "test"]);
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    // A quarantine never passes a run (ADR 0034).
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("FAIL example.com/m::TestF (quarantined): failed every time")
            && !out.contains("FAIL TestF"),
        "{out}"
    );
}

/// Round 15: a slow `go test -v` test was listed twice among the slow
/// tests, under its provisional name and its package's.
#[test]
fn a_slow_go_test_is_listed_once() {
    let d = workspace("gov-slow");
    let script = format!(
        "{WORK}printf '%s\\n' '=== RUN   TestSlow' '--- PASS: TestSlow (1.50s)' 'PASS' 'ok  \texample.com/m\t1.5s'"
    );
    let o = prova(&d, &["run", "--", "sh", "-c", &script]);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(
        out.contains("slow tests: example.com/m::TestSlow 1.5s (prova slowest)"),
        "{out}"
    );
    let o = prova(&d, &["slowest"]);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(
        !out.contains("\nTestSlow") && !out.starts_with("TestSlow"),
        "{out}"
    );
}

/// Round 15: with `go test -v` text under gotestsum `--rerun-fails`, the
/// provisional name's failure stayed in the verdict after the rerun passed:
/// FAILED with exit 0.
#[test]
fn a_go_verbose_rerun_that_passes_is_flaky_not_failed() {
    let d = workspace("gov-rerun");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' '=== RUN   TestOK' '--- PASS: TestOK (0.00s)' '=== RUN   TestF' '    f_test.go:4: flake' \
         '--- FAIL: TestF (0.00s)' 'FAIL' 'FAIL\texample.com/m\t0.01s' \
         '=== RUN   TestF' '--- PASS: TestF (0.00s)' 'PASS' 'ok  \texample.com/m\t0.01s'; exit 0",
    );
    assert_eq!(code, Some(0), "{s}");
    assert_eq!(s["outcome"], "passed", "{s}");
    assert_eq!(s["verified"], false, "{s}");
}

/// Round 15: gotestsum `--format testname` prints the module's root package
/// with no package (`PASS TestAdd (0.00s)`, `FAIL .`): its passes were
/// dropped and `FAIL TestDiv` was read as a package. Real gotestsum output.
#[test]
fn gotestsum_testname_root_package_tests_are_tests() {
    let d = workspace("gts-root");
    // go.mod names the root package as `go test` prints it.
    std::fs::write(d.join("go.mod"), "module example.com/calc\n\ngo 1.22\n").unwrap();
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' 'EMPTY cmd/conform' 'PASS TestAdd (0.00s)' '=== RUN   TestDiv' \
         '    calc_test.go:17: Div(8, 2) = 16, want 4' '--- FAIL: TestDiv (0.00s)' 'FAIL TestDiv (0.00s)' \
         'PASS TestFlakyClock (0.00s)' 'PASS TestSlowIntegration (1.50s)' 'SKIP TestHang (0.00s)' 'FAIL .' '' \
         '=== Skipped' '=== SKIP: . TestHang (0.00s)' '    calc_test.go:33: set HANG=1 to hang' '' \
         '=== Failed' '=== FAIL: . TestDiv (0.00s)' '    calc_test.go:17: Div(8, 2) = 16, want 4' '' \
         'DONE 5 tests, 1 skipped, 1 failure in 1.503s'; exit 1",
    );
    assert_eq!(code, Some(1), "{s}");
    assert_eq!(failing(&s), vec!["example.com/calc::TestDiv"], "{s}");
    assert_eq!(
        (
            s["counts"]["passed"].as_u64(),
            s["counts"]["skipped"].as_u64(),
            s["counts"]["errored"].as_u64()
        ),
        (Some(3), Some(1), Some(0)),
        "{s}"
    );
}

/// Round 16: selecting `pkg/b::TestAdd` ran `go test -run ^TestAdd$ ./...`,
/// so pkg/a's TestAdd ran (and failed) too, with no word that it was not
/// what was selected.
#[test]
fn a_qualified_selection_names_same_named_tests_it_also_ran() {
    let d = workspace("qual-sel");
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.go]\nrun = \"true\"\nselect = {:?}\n",
            format!(
                "{WORK}: go test -run {{filter}} ./...; printf '%s\\n' '=== RUN   TestAdd' '    a_test.go:3: boom' '--- FAIL: TestAdd (0.00s)' 'FAIL' 'FAIL\texample.com/m/a\t0.01s' '=== RUN   TestAdd' '--- PASS: TestAdd (0.00s)' 'PASS' 'ok  \texample.com/m/b\t0.01s'; exit 1"
            )
        ),
    )
    .unwrap();
    let o = prova(&d, &["run", "go", "--select", "example.com/m/b::TestAdd"]);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(
        out.contains(
            "outside the selection \"example.com/m/b::TestAdd\": example.com/m/a::TestAdd"
        ),
        "{out}"
    );
}

/// Round 16: the parent of failing subtests showed Go's framing lines as
/// its message (`go test -json`, as gotestsum's stream gives it).
#[test]
fn a_parent_of_failing_subtests_has_no_framing_message() {
    let d = workspace("parent-msg");
    let ev = |a: &str, t: &str, out: Option<&str>| {
        let mut v = serde_json::json!({"Action": a, "Package": "example.com/m", "Test": t});
        if let Some(o) = out {
            v["Output"] = format!("{o}\n").into();
        }
        format!("'{v}'")
    };
    let lines = [
        ev("run", "TestMean", None),
        ev("output", "TestMean", Some("=== RUN   TestMean")),
        ev("run", "TestMean/one", None),
        ev("output", "TestMean/one", Some("=== RUN   TestMean/one")),
        ev(
            "output",
            "TestMean/one",
            Some("    m_test.go:9: got 2 want 1"),
        ),
        ev(
            "output",
            "TestMean/one",
            Some("    --- FAIL: TestMean/one (0.00s)"),
        ),
        ev("fail", "TestMean/one", None),
        ev("output", "TestMean", Some("--- FAIL: TestMean (0.00s)")),
        ev("fail", "TestMean", None),
    ];
    let (_, s) = run_sh(&d, &format!("printf '%s\\n' {}; exit 1", lines.join(" ")));
    let parent = s["failures"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["test"] == "example.com/m::TestMean")
        .cloned()
        .unwrap_or_default();
    let msg = parent["message"].as_str().unwrap_or("");
    assert!(!msg.contains("=== RUN") && !msg.contains("--- FAIL"), "{s}");
}

/// Round 16: a quarantined `go test -v` failure made the pass "unverified":
/// "1 test(s) failed, then passed on a retry: TestFlaky", with no retry.
#[test]
fn a_quarantined_go_failure_is_no_retry() {
    let d = workspace("q-noretry");
    let script = format!(
        "{WORK}printf '%s\\n' '=== RUN   TestOK' '--- PASS: TestOK (0.00s)' '=== RUN   TestFlaky' '    f_test.go:4: tick' '--- FAIL: TestFlaky (0.00s)' 'FAIL' 'FAIL\texample.com/m\t0.01s' 'FAIL'; exit 1"
    );
    fake_go(&d, &script);
    prova(&d, &["run", "--", "./rc/go", "test"]);
    prova(
        &d,
        &[
            "quarantine",
            "add",
            "example.com/m::TestFlaky",
            "--reason",
            "clock",
        ],
    );
    let o = prova(&d, &["run", "--json", "--", "./rc/go", "test"]);
    let s: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    // A quarantine never passes a run (ADR 0034), and its failure is no
    // retry either.
    assert_eq!(s["outcome"], "failed", "{s}");
    assert!(!s.to_string().contains("passed on a retry"), "{s}");
}

/// Round 16: gotestsum prints a subpackage relative to the module
/// (`calc.TestFlaky`), `go test` by its import path: one test, two names.
#[test]
fn gotestsum_subpackages_are_named_by_import_path() {
    let d = workspace("gts-subpkg");
    std::fs::write(d.join("go.mod"), "module example.com/m\n").unwrap();
    let (_, s) = run_sh(
        &d,
        "printf '%s\\n' 'PASS calc.TestAdd (0.00s)' 'FAIL calc.TestFlaky (0.00s)' 'FAIL calc' '' 'DONE 2 tests, 1 failure in 0.1s'; exit 1",
    );
    assert_eq!(failing(&s), vec!["example.com/m/calc::TestFlaky"], "{s}");
}

/// Round 16: gotestsum `--format pkgname --rerun-fails` whose rerun passed
/// (real capture: the failures are listed, the package's last line is
/// green, exit 0) was reported as a failure.
#[test]
fn a_gotestsum_pkgname_rerun_that_passed_is_flaky() {
    let d = workspace("gts-pkg-rerun");
    std::fs::write(d.join("go.mod"), "module example.com/calc\n").unwrap();
    let capture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/quarantine/gotestsum-pkgname-rerun.txt"
    );
    let (code, s) = run_sh(&d, &format!("cat {capture}; exit 0"));
    assert_eq!(code, Some(0), "{s}");
    assert_eq!(s["outcome"], "passed", "{s}");
    assert!(s.to_string().contains("passed on a retry"), "{s}");
}
