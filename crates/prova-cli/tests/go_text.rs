//! Medium tests for `go test` and gotestsum text output (round 14): the
//! package is part of a test's identity, repeated executions are attempts,
//! reruns that pass are flaky, and a failed package is never covered.
//! Real processes: the verdict combines the exit code, the output and the
//! store's history.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

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

/// `go test -count=3`: pass, fail, pass of one test is one test with three
/// attempts, flaky, and its history keeps the failure.
#[test]
fn go_count_repeats_keep_the_failure_in_the_history() {
    let d = workspace("count");
    let (code, s) = run_sh(
        &d,
        "printf '%s\\n' '=== RUN   TestRace' '--- PASS: TestRace (0.00s)' '=== RUN   TestRace' \
         '    race_test.go:9: lost update' '--- FAIL: TestRace (0.00s)' '=== RUN   TestRace' \
         '--- PASS: TestRace (0.00s)' 'FAIL' 'FAIL\texample.com/m/r\t0.01s' 'FAIL'; exit 1",
    );
    assert_eq!(code, Some(1), "the command failed: {s}");
    assert_eq!(s["outcome"], "failed", "{s}");
    assert!(!s.to_string().contains("second result"), "{s}");
    let h = text(&prova(&d, &["history", "TestRace"]));
    assert!(
        h.contains("example.com/m/r::TestRace")
            && h.contains("3 attempts")
            && h.contains("lost update")
            && h.contains("FLAKY"),
        "{h}"
    );
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
