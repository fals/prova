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
         own) echo 'test calc::x ... FAILED'; echo \"thread 'calc::x' panicked at src/lib.rs:9:5:\"; exit 101 ;; \
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
    let idle = "echo 'test calc::y ... ok'".to_owned();
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
