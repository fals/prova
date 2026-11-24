//! Medium tests for ADR 0027 (usability round 13): quarantine of known-flaky
//! tests, the warning for a flaky test left out of the command, and slow
//! tests. Real runs are needed: both come from the store's history across
//! suites. A medium test because a smaller one cannot run the CLI end to end.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-q-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(".gitignore"), "state\nn\n").unwrap();
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

/// Some real work first: prova does not believe results printed by a
/// command that used no CPU (round 10). `calc::ok` and `calc::y` always
/// pass; `calc::x` follows the file `state`.
const BUSY: &str = "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done;";
fn full() -> String {
    format!(
        "{BUSY} echo 'test calc::ok ... ok'; echo 'test calc::y ... ok'; if [ \"$(cat state)\" = pass ]; then echo 'test calc::x ... ok'; else echo 'test calc::x ... FAILED'; exit 1; fi"
    )
}
/// The same suite as `full`, with the flaky test left out of the command.
fn without_x() -> String {
    format!("{BUSY} echo 'test calc::ok ... ok'; echo 'test calc::y ... ok'")
}

fn suites(d: &Path) {
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.make]\nrun = {:?}\n\n[suite.stable]\nrun = {:?}\n",
            full(),
            without_x()
        ),
    )
    .unwrap();
}

fn run(d: &Path, suite: &str, state: &str) -> Output {
    std::fs::write(d.join("state"), state).unwrap();
    prova(d, &["run", suite])
}

/// `calc::x` is known to prova and flaky: it passed and failed on one code.
fn flaky_history(d: &Path) {
    suites(d);
    run(d, "make", "pass");
    run(d, "make", "fail");
}

#[test]
fn a_quarantined_test_that_fails_does_not_fail_the_run_and_is_listed() {
    let d = workspace("fails");
    flaky_history(&d);
    let o = prova(
        &d,
        &["quarantine", "add", "calc::x", "--reason", "clock tick"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let o = run(&d, "make", "fail");
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(
        out.contains("PASSED") && !out.contains("unverified"),
        "verified for the rest: {out}"
    );
    assert!(
        out.contains("quarantined")
            && out.contains("calc::x")
            && out.contains("failed")
            && out.contains("clock tick"),
        "{out}"
    );
    // History keeps the failure: quarantine hides nothing from it.
    let h = text(&prova(&d, &["history", "calc::x"]));
    assert!(h.contains("fail"), "{h}");
}

#[test]
fn a_quarantined_pass_is_verified_and_still_listed() {
    let d = workspace("passes");
    flaky_history(&d);
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    let out = text(&run(&d, "make", "pass"));
    assert!(
        out.contains("PASSED") && !out.contains("unverified") && !out.contains("known flaky"),
        "{out}"
    );
    assert!(
        out.contains("quarantined") && out.contains("calc::x"),
        "{out}"
    );
}

#[test]
fn another_failure_next_to_a_quarantined_one_still_fails_the_run() {
    let d = workspace("other");
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.make]\nrun = {:?}\n",
            format!(
                "{BUSY} if [ \"$(cat state)\" = pass ]; then echo 'test calc::x ... ok'; echo 'test calc::z ... ok'; else echo 'test calc::x ... FAILED'; echo 'test calc::z ... FAILED'; exit 1; fi"
            )
        ),
    )
    .unwrap();
    run(&d, "make", "pass");
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    let o = run(&d, "make", "fail");
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("FAIL calc::z"), "{out}");
}

#[test]
fn quarantine_list_remove_and_unknown_tests() {
    let d = workspace("cli");
    flaky_history(&d);
    let o = prova(&d, &["quarantine", "add", "calc::nope", "--reason", "r"]);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    prova(
        &d,
        &["quarantine", "add", "calc::x", "--reason", "clock tick"],
    );
    let l = text(&prova(&d, &["quarantine", "list"]));
    assert!(l.contains("calc::x") && l.contains("clock tick"), "{l}");
    let j: serde_json::Value =
        serde_json::from_str(&text(&prova(&d, &["quarantine", "list", "--json"]))).unwrap();
    assert_eq!(j[0]["test"], "calc::x");
    let o = prova(&d, &["quarantine", "remove", "calc::x"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let l = text(&prova(&d, &["quarantine", "list"]));
    assert!(
        !l.contains("calc::x") && l.contains("no quarantined"),
        "{l}"
    );
    // Back to counting: a known flaky pass is unverified again.
    let out = text(&run(&d, "make", "pass"));
    assert!(out.contains("known flaky"), "{out}");
}

/// Round 13: excluding the flaky test in the command gave a clean verified
/// pass, because the new suite had no history of its own.
#[test]
fn leaving_a_known_flaky_test_out_of_another_suite_is_warned_about() {
    let d = workspace("leftout");
    flaky_history(&d);
    let out = text(&run(&d, "stable", "pass"));
    assert!(
        out.contains("PASSED (unverified")
            && out.contains("1 known flaky test(s) did not run: calc::x"),
        "{out}"
    );
    // Setting it aside openly is accepted, and listed as not run.
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    let out = text(&run(&d, "stable", "pass"));
    assert!(
        out.contains("PASSED") && !out.contains("unverified") && !out.contains("did not run"),
        "{out}"
    );
    assert!(
        out.contains("quarantined") && out.contains("not run"),
        "{out}"
    );
}

/// Round 13: nothing listed slow tests. The script really takes over a
/// second and a half, so the reported 1.5 s is possible (a longer test than the run is
/// an impossible result, ADR 0022).
#[test]
fn slow_tests_are_named_in_the_summary_and_listed_by_slowest() {
    let d = workspace("slow");
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.make]\nrun = {:?}\n",
            format!(
                "{BUSY} sleep 1.7; echo '--- PASS: TestSlow (1.50s)'; echo '--- PASS: TestFast (0.01s)'"
            )
        ),
    )
    .unwrap();
    let out = text(&prova(&d, &["run", "make"]));
    assert!(out.contains("slow tests: TestSlow 1.5s"), "{out}");
    assert!(!out.contains("TestFast 0"), "{out}");
    let s = text(&prova(&d, &["slowest"]));
    let slow = s.find("TestSlow").expect("listed");
    let fast = s.find("TestFast").expect("listed");
    assert!(slow < fast, "worst first: {s}");
    assert!(s.contains("p50") && s.contains("p95"), "{s}");
    let j: serde_json::Value =
        serde_json::from_str(&text(&prova(&d, &["slowest", "-n", "1", "--json"]))).unwrap();
    assert_eq!(j.as_array().unwrap().len(), 1);
    assert_eq!(j[0]["test"], "TestSlow");
}

/// Round 14: a quarantined failure passed a run whose output also showed a
/// build failure, a data race or more failures than the quarantined test's;
/// and the quarantined test's own panic made a quarantined pass unverified.
#[test]
fn a_quarantine_sets_aside_only_what_its_test_explains() {
    let d = workspace("explains");
    let script = format!(
        "{BUSY} echo 'test calc::ok ... ok'; case \"$(cat state)\" in \
         pass) echo 'test calc::x ... ok' ;; \
         own) echo 'test calc::x ... FAILED'; echo 'failures:'; echo '---- calc::x stdout ----'; echo \"thread 'calc::x' panicked at src/lib.rs:9:5:\"; exit 101 ;; \
         summary) echo 'test calc::x ... FAILED'; echo 'error: test run failed'; echo 'error: test failed, to rerun pass `--lib`'; exit 100 ;; \
         race) echo 'test calc::x ... FAILED'; echo 'WARNING: DATA RACE'; exit 101 ;; \
         build) echo 'test calc::x ... FAILED'; echo 'error[E0425]: cannot find value `y` in this scope'; exit 101 ;; \
         more) echo 'test calc::x ... FAILED'; echo 'test result: FAILED. 1 passed; 3 failed; 0 ignored'; exit 101 ;; \
         esac"
    );
    std::fs::write(
        d.join("prova.toml"),
        format!("[suite.make]\nrun = {script:?}\n"),
    )
    .unwrap();
    run(&d, "make", "pass");
    run(&d, "make", "own");
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    let o = run(&d, "make", "own");
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(!out.contains("unverified"), "its own panic: {out}");
    // Round 15: the runner's own "tests failed" summary is the quarantined
    // failure's, not something else that failed.
    let o = run(&d, "make", "summary");
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "runner summary: {out}");
    for (state, why) in [
        ("race", "WARNING: DATA RACE"),
        ("build", "error[E0425]"),
        ("more", "counted 3 failure(s)"),
    ] {
        let o = run(&d, "make", state);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{state}: {out}");
        assert!(
            out.contains("FAILED") && out.contains(why),
            "{state}: {out}"
        );
    }
}

/// Round 14: a bare name seen once (gotestsum's text) and the same test
/// qualified (its json stream) were two tests; `add` set aside the stale
/// bare one, and the real failure still failed the run.
#[test]
fn a_bare_name_with_a_qualified_twin_is_ambiguous() {
    let d = workspace("twin");
    flaky_history(&d);
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.make]\nrun = {:?}\n\n[suite.bare]\nrun = {:?}\n",
            full(),
            format!("{BUSY} echo 'test x ... FAILED'; exit 1")
        ),
    )
    .unwrap();
    prova(&d, &["run", "bare"]);
    let o = prova(&d, &["quarantine", "add", "x", "--reason", "r"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(2), "{out}");
    assert!(out.contains("also known qualified, as calc::x"), "{out}");
    let o = prova(&d, &["quarantine", "add", "calc::x", "--reason", "r"]);
    let out = text(&o);
    assert!(out.contains("this machine's prova store"), "{out}");
}

/// Round 14: a pass that set a quarantined failure aside looked like any
/// pass in the run list and in `--repeat`; a run of only the quarantined
/// test, failing, passed.
#[test]
fn a_set_aside_failure_is_marked_everywhere_and_never_the_only_result() {
    let d = workspace("marked");
    flaky_history(&d);
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    std::fs::write(d.join("state"), "fail").unwrap();
    let o = prova(&d, &["run", "make", "--repeat", "2"]);
    let out = text(&o);
    assert!(
        out.contains("(quarantined, set aside: calc::x)")
            && out.contains("FAIL calc::x (quarantined): failed every time"),
        "{out}"
    );
    let list = text(&prova(&d, &["status"]));
    assert!(
        list.contains("passed*") && list.contains("passed*: passed with quarantined tests failing"),
        "{list}"
    );
    let last = text(&prova(&d, &["status", "last"]));
    assert!(
        last.contains("PASSED (quarantined failures set aside)"),
        "{last}"
    );
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.only]\nrun = {:?}\n",
            format!("{BUSY} echo 'test calc::x ... FAILED'; exit 101")
        ),
    )
    .unwrap();
    let o = prova(&d, &["run", "only"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    // Round 15: and it says why.
    assert!(
        text(&o).contains("only quarantined test(s) failed (calc::x), and no other test passed"),
        "{}",
        text(&o)
    );
}

/// Round 15: `--repeat` over quiet output said a test that failed once in
/// five runs "failed every time" and that four runs "ran no test"; over
/// passes that proved little (cached results) it said "every test passed
/// every time".
#[test]
fn repeat_tallies_say_what_was_not_seen() {
    let d = workspace("repeat-quiet");
    let quiet = format!(
        "{BUSY} n=$(cat n 2>/dev/null || echo 0); n=$((n+1)); echo $n > n; \
         if [ $((n % 2)) -eq 1 ]; then echo 'test calc::x ... FAILED'; echo 'test result: FAILED. 2 passed; 1 failed; 0 ignored'; exit 101; \
         else echo 'test result: ok. 3 passed; 0 failed; 0 ignored'; fi"
    );
    // Every test skipped: a pass that proved little on every platform (the
    // no-CPU check needs Linux's child CPU times).
    let idle = format!("{BUSY} echo 'test calc::y ... ignored'");
    std::fs::write(
        d.join("prova.toml"),
        format!("[suite.quiet]\nrun = {quiet:?}\n\n[suite.idle]\nrun = {idle:?}\n"),
    )
    .unwrap();
    let out = text(&prova(&d, &["run", "quiet", "--repeat", "4"]));
    assert!(
        out.contains("failed each of the 2 time(s) it was seen; no result in the other 2 run(s)")
            && !out.contains("ran no test")
            && !out.contains("failed every time"),
        "{out}"
    );
    let out = text(&prova(&d, &["run", "idle", "--repeat", "2"]));
    assert!(
        out.contains("2 of 2 pass(es) verified little")
            && !out.contains("every test passed every time"),
        "{out}"
    );
}

/// Round 15: a pass that set a quarantined failure aside was named "last
/// green", and every --select run listed the quarantined test "not run".
#[test]
fn a_set_aside_pass_is_no_last_green_and_selections_stay_quiet() {
    let d = workspace("lastgreen");
    flaky_history(&d);
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    let first = text(&run(&d, "make", "fail"));
    let set_aside = first.split_whitespace().nth(1).unwrap().to_owned();
    let out = text(&run(&d, "make", "fail"));
    assert!(
        out.contains("PASSED (quarantined failures set aside)"),
        "{out}"
    );
    assert!(
        !out.contains(&format!("last green: run {set_aside}")),
        "{set_aside}: {out}"
    );
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.make]\nrun = {:?}\nselect = {:?}\n",
            full(),
            format!("{BUSY} echo 'test calc::y ... ok'")
        ),
    )
    .unwrap();
    let out = text(&prova(&d, &["run", "make", "--select", "calc::y"]));
    assert!(!out.contains("quarantined:"), "{out}");
}

/// Round 15: the guard matched a short list of crash lines; a coverage
/// gate, a type checker, another make target, a killed run, an interrupted
/// session or a crash of `test_queue` (next to quarantined `test_q`)
/// passed behind a quarantined failure. Every error line must now be
/// accounted for (ADR 0029).
#[test]
fn a_quarantine_accounts_for_every_error_line() {
    let d = workspace("accounts");
    let pytest = "printf '%s\\n' 'collected 3 items' \
        'tests/test_a.py::test_q FAILED      [ 33%]' \
        'tests/test_a.py::test_queue PASSED  [ 66%]' \
        'tests/test_a.py::test_other PASSED  [100%]' \
        '=================================== FAILURES ===================================' \
        '___________________________________ test_q ____________________________________' \
        '    def test_q():' \
        '>       assert 2 == 1' \
        'E       assert 2 == 1' \
        'tests/test_a.py:3: AssertionError' \
        '=========================== short test summary info ============================' \
        'FAILED tests/test_a.py::test_q - assert 2 == 1' \
        '========================= 1 failed, 2 passed in 0.12s =========================='";
    let script = format!(
        "{BUSY} : make test; {pytest}; case \"$(cat state)\" in \
         plain) echo 'make: *** [Makefile:2: test] Error 1'; exit 2 ;; \
         coverage) echo 'FAIL Required test coverage of 90% not reached. Total coverage: 50.00%'; exit 1 ;; \
         tsc) echo \"src/app.ts(3,7): error TS2322: Type 'string' is not assignable to type 'number'.\"; exit 1 ;; \
         lint) echo 'make: *** [Makefile:5: lint] Error 1'; exit 2 ;; \
         killed) exit 137 ;; \
         segv) echo 'tests/test_a.py::test_queue Segmentation fault (core dumped)'; exit 1 ;; \
         interrupted) echo '!!!!!!!!!!!!!!!!!!! Interrupted: 1 error during collection !!!!!!!!!!!!!!!!!!!'; exit 2 ;; \
         esac"
    );
    std::fs::write(
        d.join("prova.toml"),
        format!("[suite.make]\nrun = {script:?}\n"),
    )
    .unwrap();
    run(&d, "make", "plain");
    prova(
        &d,
        &[
            "quarantine",
            "add",
            "tests.test_a::test_q",
            "--reason",
            "flake",
        ],
    );
    let o = run(&d, "make", "plain");
    assert_eq!(o.status.code(), Some(0), "plain: {}", text(&o));
    for state in ["coverage", "tsc", "lint", "killed", "segv", "interrupted"] {
        let o = run(&d, "make", state);
        assert_eq!(o.status.code(), Some(1), "{state}: {}", text(&o));
    }
}

/// Round 15: in `go test -json`, a panic in the package's own output after
/// the tests passed behind a quarantined failure.
#[test]
fn a_package_panic_in_go_json_is_not_a_quarantined_tests() {
    let d = workspace("gojson-panic");
    let ev = |a: &str, t: Option<&str>, out: Option<&str>| {
        let mut v = serde_json::json!({"Action": a, "Package": "example.com/q"});
        if let Some(t) = t {
            v["Test"] = t.into();
        }
        if let Some(o) = out {
            v["Output"] = format!("{o}\n").into();
        }
        format!("'{}'", v)
    };
    let lines = |panic: bool| {
        let mut l = vec![
            ev("run", Some("TestQueue"), None),
            ev("pass", Some("TestQueue"), None),
            ev("run", Some("TestQ"), None),
            ev("output", Some("TestQ"), Some("=== RUN   TestQ")),
            ev("output", Some("TestQ"), Some("    q_test.go:3: flake")),
            ev("fail", Some("TestQ"), None),
        ];
        if panic {
            l.push(ev("output", None, Some("panic: send on closed channel")));
            l.push(ev("output", None, Some("goroutine 12 [running]:")));
        }
        l.push(ev("output", None, Some("FAIL\texample.com/q\t0.01s")));
        l.push(ev("fail", None, None));
        format!("{BUSY} printf '%s\\n' {}; exit 1", l.join(" "))
    };
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.plain]\nrun = {:?}\n\n[suite.panic]\nrun = {:?}\n",
            lines(false),
            lines(true)
        ),
    )
    .unwrap();
    prova(&d, &["run", "plain"]);
    prova(
        &d,
        &[
            "quarantine",
            "add",
            "example.com/q::TestQ",
            "--reason",
            "flake",
        ],
    );
    let o = prova(&d, &["run", "plain"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let o = prova(&d, &["run", "panic"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
}

/// Round 15: a quarantine refused for a new subtest said nothing of why.
#[test]
fn a_refused_quarantine_says_why() {
    let d = workspace("why");
    let go = |sub: bool| {
        let subtest = if sub {
            "'=== RUN   TestQ/new' '    q_test.go:9: real regression' '    --- FAIL: TestQ/new (0.00s)'"
        } else {
            "'    q_test.go:3: flake'"
        };
        format!(
            "{BUSY} printf '%s\\n' '=== RUN   TestOK' '--- PASS: TestOK (0.00s)' '=== RUN   TestQ' {subtest} '--- FAIL: TestQ (0.00s)' 'FAIL' 'FAIL\texample.com/m\t0.01s' 'FAIL'; exit 1"
        )
    };
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.old]\nrun = {:?}\n\n[suite.new]\nrun = {:?}\n",
            go(false),
            go(true)
        ),
    )
    .unwrap();
    prova(&d, &["run", "old"]);
    prova(
        &d,
        &[
            "quarantine",
            "add",
            "example.com/m::TestQ",
            "--reason",
            "flake",
        ],
    );
    let o = prova(&d, &["run", "new"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains(
            "first seen after their parent's quarantine failed (example.com/m::TestQ/new)"
        ),
        "{out}"
    );
}

/// Round 15: a narrow suite (unit tests) was warned that a flaky test of
/// another module "did not run", because a suite that runs everything
/// shares tests with both.
#[test]
fn a_narrow_suite_is_not_warned_about_another_modules_flaky_test() {
    let d = workspace("narrow");
    let all = format!(
        "{BUSY} echo 'test other::y ... ok'; if [ \"$(cat state)\" = pass ]; then echo 'test calc::x ... ok'; else echo 'test calc::x ... FAILED'; exit 1; fi"
    );
    let narrow = format!("{BUSY} echo 'test other::y ... ok'");
    std::fs::write(
        d.join("prova.toml"),
        format!("[suite.all]\nrun = {all:?}\n\n[suite.narrow]\nrun = {narrow:?}\n"),
    )
    .unwrap();
    run(&d, "all", "pass");
    run(&d, "all", "fail");
    let out = text(&run(&d, "narrow", "pass"));
    assert!(!out.contains("did not run"), "{out}");
}

/// Round 16: under gotestsum's default output, its red package line for
/// the quarantined test's package was "an error outside the quarantined
/// test(s)", so a quarantine never applied; and a pass behind make's stop
/// at the test step did not say later steps never ran.
#[test]
fn a_quarantine_applies_under_gotestsum_and_says_what_make_skipped() {
    let d = workspace("gts-q");
    let script = |flaky_fails: bool| {
        let verdict = if flaky_fails { "fail" } else { "pass" };
        let ev = |test: Option<&str>| match test {
            Some(t) => format!(
                r#"{{"Action":"{verdict}","Package":"example.com/calc","Test":"{t}","Elapsed":0}}"#
            ),
            None => format!(r#"{{"Action":"{verdict}","Package":"example.com/calc","Elapsed":0}}"#),
        };
        let pass_add =
            r#"{"Action":"pass","Package":"example.com/calc","Test":"TestAdd","Elapsed":0}"#;
        let json = format!(
            "printf '%s\\n' '{pass_add}' '{}' '{}' >> \"$GOTESTSUM_JSONFILE\";",
            ev(Some("TestFlaky")),
            ev(None)
        );
        let text = if flaky_fails {
            "printf '%s\\n' '\u{2716}  . (1.503s)' '' '=== Failed' '=== FAIL: . TestFlaky (0.00s)' '    calc_test.go:23: tick' '' 'DONE 2 tests, 1 failure in 0.1s' 'make: *** [Makefile:2: test] Error 1'; exit 2"
        } else {
            "printf '%s\\n' '\u{2713}  . (1.503s)' '' 'DONE 2 tests in 0.1s'"
        };
        format!("{BUSY} : make test; {json} {text}")
    };
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.flake]\nrun = {:?}\ninject = [\"gotestsum-json\"]\n\n[suite.ok]\nrun = {:?}\ninject = [\"gotestsum-json\"]\n",
            script(true),
            script(false)
        ),
    )
    .unwrap();
    std::fs::write(d.join("go.mod"), "module example.com/calc\n").unwrap();
    prova(&d, &["run", "ok"]);
    prova(&d, &["run", "flake"]);
    prova(
        &d,
        &[
            "quarantine",
            "add",
            "example.com/calc::TestFlaky",
            "--reason",
            "clock",
        ],
    );
    let o = prova(&d, &["run", "flake"]);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(0), "{t}");
    assert!(t.contains("any step after it did not run"), "{t}");
}

/// Round 16: under a quiet suite (`cargo test --quiet` via make) a refused
/// quarantine said "no other test passed" next to "the framework counted 5".
#[test]
fn a_refusal_under_quiet_output_says_passes_were_only_counted() {
    let d = workspace("quiet-q");
    let quiet = format!(
        "{BUSY} if [ \"$(cat state)\" = pass ]; then echo 'test result: ok. 5 passed; 0 failed; 0 ignored'; else echo 'test calc::x ... FAILED'; echo 'test result: FAILED. 4 passed; 1 failed; 0 ignored'; exit 101; fi"
    );
    std::fs::write(
        d.join("prova.toml"),
        format!("[suite.q]\nrun = {quiet:?}\n"),
    )
    .unwrap();
    run(&d, "q", "fail");
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    let o = run(&d, "q", "fail");
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(
        t.contains("the framework counted 5 test(s), but its output names no passing test"),
        "{t}"
    );
}

/// Round 16: history said "none of them passed" when earlier runs had
/// passed with a quarantined failure set aside.
#[test]
fn history_counts_passes_that_set_a_quarantine_aside() {
    let d = workspace("setaside-hist");
    suites(&d);
    run(&d, "make", "fail");
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    run(&d, "make", "fail");
    let out = text(&run(&d, "make", "fail"));
    assert!(
        out.contains(
            "none passed with every test passing; 2 passed with quarantined failures set aside"
        ) || out.contains("1 passed with quarantined failures set aside"),
        "{out}"
    );
    assert!(!out.contains("none of them passed"), "{out}");
}

/// Round 16: two crates' `tests::flaky` shared one name; a quarantine of
/// crate calc's set aside crate geo's real failure.
#[test]
fn a_quarantine_names_the_binary_when_a_name_is_shared() {
    let d = workspace("crates");
    let nx = |calc: &str, geo: &str, code: u8| {
        format!(
            "{BUSY} printf '%s\\n' '    Starting 3 tests across 2 binaries' '        PASS [   0.004s] calc tests::adds' '        {calc} [   0.004s] calc tests::flaky' '        {geo} [   0.004s] geo tests::flaky' '     Summary [   0.020s] 3 tests run'; exit {code}"
        )
    };
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.a]\nrun = {:?}\n\n[suite.b]\nrun = {:?}\n",
            nx("FAIL", "PASS", 100),
            nx("PASS", "FAIL", 100)
        ),
    )
    .unwrap();
    prova(&d, &["run", "a"]);
    let o = prova(
        &d,
        &["quarantine", "add", "tests::flaky", "--reason", "clock"],
    );
    let t = text(&o);
    assert_eq!(o.status.code(), Some(2), "{t}");
    assert!(
        t.contains("2 tests are named \"tests::flaky\", in: calc, geo"),
        "{t}"
    );
    let o = prova(
        &d,
        &[
            "quarantine",
            "add",
            "tests::flaky",
            "--in",
            "calc",
            "--reason",
            "clock",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let o = prova(&d, &["run", "a"]);
    assert_eq!(o.status.code(), Some(0), "calc's own: {}", text(&o));
    let o = prova(&d, &["run", "b"]);
    assert_eq!(o.status.code(), Some(1), "geo's failure: {}", text(&o));
}

/// Round 16: `quarantine add` accepted a file-level entry, which a
/// quarantine can never set aside.
#[test]
fn a_file_level_entry_cannot_be_quarantined() {
    let d = workspace("fileentry");
    let script = format!(
        "{BUSY} printf '%s\\n' 'ERROR tests/test_db.py - ModuleNotFoundError: No module named x' '1 error in 0.1s'; exit 2"
    );
    prova(&d, &["run", "--", "sh", "-c", &script]);
    let o = prova(
        &d,
        &[
            "quarantine",
            "add",
            "tests/test_db.py::(file)",
            "--reason",
            "r",
        ],
    );
    let t = text(&o);
    assert_eq!(o.status.code(), Some(2), "{t}");
    assert!(t.contains("is not a test but a file"), "{t}");
}

/// Round 16: `prova flaky` had no column names and did not show a
/// quarantine.
#[test]
fn flaky_names_its_columns_and_quarantines() {
    let d = workspace("flakycols");
    flaky_history(&d);
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    let out = text(&prova(&d, &["flaky"]));
    assert!(
        out.contains("same-code flips") && out.contains("calc::x  (quarantined)"),
        "{out}"
    );
}
