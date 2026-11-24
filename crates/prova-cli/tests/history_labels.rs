//! Medium tests for ADR 0018: honest history labels. Real runs are needed:
//! labels come from the store's history across suites and run statuses.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

/// A git repository whose `state` file (what the test script reads) is
/// ignored: runs that only change it are the same code. Outside git every
/// run is a code version of its own (round 12).
fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-hl-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(".gitignore"), "state\n").unwrap();
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
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Prints one libtest line whose outcome comes from the file `state`, after
/// a little real work: prova does not believe results printed by a command
/// that used no CPU at all (round 10).
const SCRIPT: &str = "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; if [ \"$(cat state)\" = pass ]; then echo 'test calc::x ... ok'; else echo 'test calc::x ... FAILED'; exit 1; fi";

fn suites(d: &Path) {
    std::fs::write(
        d.join("prova.toml"),
        format!("[suite.make]\nrun = {SCRIPT:?}\n\n[suite.default]\nrun = {SCRIPT:?}\n"),
    )
    .unwrap();
}

fn run(d: &Path, suite: &str, state: &str) -> String {
    std::fs::write(d.join("state"), state).unwrap();
    text(&prova(d, &["run", suite]))
}

#[test]
fn a_first_failure_without_history_is_not_called_new() {
    let d = workspace("first");
    suites(&d);
    let out = run(&d, "make", "fail");
    assert!(
        out.contains("FAIL calc::x [first time seen: no history yet]"),
        "{out}"
    );
    run(&d, "make", "pass");
    let out = run(&d, "make", "fail");
    assert!(!out.contains("first time seen"), "{out}");
}

#[test]
fn history_is_shared_by_suites_that_run_the_same_test() {
    let d = workspace("shared");
    suites(&d);
    // Flaky in `make`: passes and fails on the same code.
    run(&d, "make", "pass");
    run(&d, "make", "fail");
    // In `default` it is the same test: known flaky, not new.
    let out = run(&d, "default", "fail");
    assert!(
        out.contains("FAIL calc::x [failed") && out.contains("FLAKY"),
        "{out}"
    );
    assert!(
        !out.contains("new failure") && !out.contains("first time seen"),
        "{out}"
    );
    let h = text(&prova(&d, &["history", "calc::x"]));
    assert!(h.contains("suites default, make"), "{h}");
}

#[test]
fn cancelled_runs_do_not_make_a_test_flaky() {
    let d = workspace("cancel");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.go]\nrun = \"b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; echo '=== RUN   TestA'; if [ -f hang ]; then sleep 30; fi; echo '--- PASS: TestA (0.00s)'\"\n",
    )
    .unwrap();
    assert!(prova(&d, &["run", "go"]).status.success());
    std::fs::write(d.join("hang"), "").unwrap();
    let o = prova(&d, &["run", "go", "--detach", "--stall-after", "1s"]);
    let id = text(&o).trim().to_owned();
    let w = text(&prova(
        &d,
        &["wait", &id, "--until", "stall", "--max-wait", "15s"],
    ));
    assert!(
        w.contains("STALLED") && w.contains("running now: TestA"),
        "{w}"
    );
    assert!(prova(&d, &["cancel", &id]).status.success());
    prova(&d, &["wait", &id, "--until", "done", "--max-wait", "10s"]);
    let flaky = text(&prova(&d, &["flaky"]));
    assert!(!flaky.contains("TestA"), "{flaky}");
    let h = text(&prova(&d, &["history", "TestA"]));
    assert!(h.contains("1 runs with a result, 0 failed"), "{h}");
}

#[test]
fn quiet_counts_never_contradict() {
    let d = workspace("quiet");
    let o = prova(
        &d,
        &[
            "run",
            "--",
            "sh",
            "-c",
            "echo 'test result: ok. 4 passed; 0 failed; 0 ignored'",
        ],
    );
    let out = text(&o);
    assert!(out.contains("tests: 0 passed, 0 failed, 0 errored, 0 skipped seen individually; the framework counted 4 (0 failed)"), "{out}");
    assert!(
        out.contains("the framework counted 4 test(s), but none was visible individually"),
        "{out}"
    );
    assert!(!out.contains("no test was observed"), "{out}");
}

/// Round 5: quiet `cargo test` names no binary and `go test` text names no
/// package; their results are still the same tests as the full streams'.
#[test]
fn quiet_and_text_runs_join_the_full_history() {
    let d = workspace("join");
    std::fs::write(
        d.join("prova.toml"),
        concat!(
            "[suite.rust_full]\nrun = \"echo '     Running unittests src/lib.rs (target/debug/deps/calc-0a1b)'; sh r.sh\"\n",
            "[suite.rust_quiet]\nrun = \"sh r.sh\"\n",
            "[suite.go_json]\nrun = \"sh g.sh\"\n",
            "[suite.go_text]\nrun = \"echo '--- FAIL: TestDiv (0.00s)'; printf 'FAIL\\\\texample.com/calc\\\\t0.01s\\\\n'; exit 1\"\n",
        ),
    )
    .unwrap();
    std::fs::write(d.join("r.sh"), "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; if [ \"$(cat state)\" = pass ]; then echo 'test tests::x ... ok'; else echo 'test tests::x ... FAILED'; exit 1; fi").unwrap();
    std::fs::write(
        d.join("g.sh"),
        "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; echo '{\"Action\":\"run\",\"Package\":\"example.com/calc\",\"Test\":\"TestDiv\"}'; echo '{\"Action\":\"pass\",\"Package\":\"example.com/calc\",\"Test\":\"TestDiv\",\"Elapsed\":0}'",
    )
    .unwrap();
    std::fs::write(d.join("state"), "pass").unwrap();
    prova(&d, &["run", "rust_full"]);
    std::fs::write(d.join("state"), "fail").unwrap();
    let out = text(&prova(&d, &["run", "rust_quiet"]));
    assert!(
        out.contains("FAIL tests::x [")
            && out.contains("last passed at")
            && !out.contains("first time seen"),
        "quiet run knows the earlier pass: {out}"
    );
    prova(&d, &["run", "go_json"]);
    let out = text(&prova(&d, &["run", "go_text"]));
    assert!(
        out.contains("TestDiv [")
            && out.contains("last passed at")
            && !out.contains("first time seen"),
        "text run knows the json pass: {out}"
    );
    // Two names, one test: history answers instead of asking (round 6).
    let h = prova(&d, &["history", "TestDiv"]);
    assert!(h.status.success(), "{}", text(&h));
}

/// Round 5: a test cut off by a cancel kept a garbled duplicate identity,
/// which `prova history` could not tell apart; it is shown as unfinished.
#[test]
fn a_cut_off_test_keeps_its_identity_and_is_shown_as_unfinished() {
    let d = workspace("cutoff");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.go]\nrun = \"echo '=== RUN   TestA'; sleep 30\"\n",
    )
    .unwrap();
    let id = text(&prova(&d, &["run", "go", "--detach"]))
        .trim()
        .to_owned();
    prova(
        &d,
        &["wait", &id, "--until", "first-failure", "--max-wait", "2s"],
    );
    assert!(prova(&d, &["cancel", &id]).status.success());
    let s = text(&prova(
        &d,
        &["wait", &id, "--until", "done", "--max-wait", "10s"],
    ));
    assert!(s.contains("UNFINISHED TestA"), "{s}");
    assert!(!s.contains("FLAKY"), "{s}");
    let h = prova(&d, &["history", "TestA"]);
    assert!(h.status.success(), "one test, not ambiguous: {}", text(&h));
}

/// Round 8: a test that failed before the run was cancelled failed on its
/// own: it keeps its label and counts. (A test killed by the cancel is
/// unfinished instead; see the round 7 test below.)
#[test]
fn a_failure_before_the_cancel_is_still_evidence() {
    let d = workspace("cancelfail");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.t]\nrun = \"if [ -f hang ]; then echo 'test calc::x ... FAILED'; sleep 30; else echo 'test calc::x ... ok'; fi\"\n",
    )
    .unwrap();
    assert!(prova(&d, &["run", "t"]).status.success());
    std::fs::write(d.join("hang"), "").unwrap();
    let id = text(&prova(&d, &["run", "t", "--detach"]))
        .trim()
        .to_owned();
    prova(
        &d,
        &["wait", &id, "--until", "first-failure", "--max-wait", "5s"],
    );
    assert!(prova(&d, &["cancel", &id]).status.success());
    let s = text(&prova(
        &d,
        &["wait", &id, "--until", "done", "--max-wait", "10s"],
    ));
    assert!(
        s.contains("FAIL calc::x [") && !s.contains("no evidence"),
        "{s}"
    );
    let h = text(&prova(&d, &["history", "calc::x"]));
    assert!(h.contains("1 failed"), "{h}");
}

/// Round 6: `prova flaky` agreed with history only within one suite.
#[test]
fn flaky_lists_a_test_that_flipped_across_suites() {
    let d = workspace("flakyacross");
    suites(&d);
    run(&d, "make", "pass");
    run(&d, "default", "fail");
    let f = text(&prova(&d, &["flaky"]));
    assert!(f.contains("calc::x"), "{f}");
}

/// Round 6: confirming a flaky test is one command, not a shell loop.
#[test]
fn repeat_tallies_each_test() {
    let d = workspace("repeat");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.t]\nrun = \"n=$(cat n 2>/dev/null || echo 0); echo $((n+1)) > n; if [ $((n % 2)) = 1 ]; then echo 'test calc::x ... FAILED'; exit 1; else echo 'test calc::x ... ok'; fi\"\n",
    )
    .unwrap();
    let o = prova(&d, &["run", "t", "--repeat", "3"]);
    assert_eq!(o.status.code(), Some(1));
    let out = text(&o);
    assert!(
        out.contains("3 run(s) of the same code") && out.contains("FLAKY calc::x: failed 1 of 3"),
        "{out}"
    );
}

/// Round 7: a green run hid a known flaky test that passed by luck, and
/// `prova diff` listed that pass as "fixed".
#[test]
fn a_known_flaky_pass_is_named_and_not_called_fixed() {
    let d = workspace("flakypass");
    suites(&d);
    run(&d, "make", "pass");
    run(&d, "make", "fail");
    let failed = text(&prova(&d, &["status", "last", "--json"]));
    let failed: serde_json::Value = serde_json::from_str(&failed).unwrap();
    let out = run(&d, "make", "pass");
    assert!(
        out.contains("PASSED (unverified: see warnings)")
            && out.contains(
                "warning: 1 known flaky test(s) passed this time, which proves little (not verified): calc::x"
            ),
        "{out}"
    );
    let diff = text(&prova(
        &d,
        &["diff", failed["run"].as_str().unwrap(), "last"],
    ));
    assert!(
        diff.contains("changed, but known flaky (1):") && !diff.contains("fixed"),
        "{diff}"
    );
}

/// Round 7: nextest reports a test it killed on cancel as failed; a test
/// that "fails" while the run is being cancelled did not finish.
#[test]
fn a_test_killed_by_the_cancel_is_unfinished_not_failed() {
    let d = workspace("killedfail");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.go]\nrun = \"trap 'echo \\\"--- FAIL: TestA (9.00s)\\\"; exit 1' TERM; echo '=== RUN   TestA'; sleep 30 & wait\"\n",
    )
    .unwrap();
    let id = text(&prova(&d, &["run", "go", "--detach"]))
        .trim()
        .to_owned();
    prova(
        &d,
        &["wait", &id, "--until", "first-failure", "--max-wait", "2s"],
    );
    assert!(prova(&d, &["cancel", &id]).status.success());
    let s = text(&prova(
        &d,
        &["wait", &id, "--until", "done", "--max-wait", "10s"],
    ));
    assert!(
        s.contains("UNFINISHED TestA") && !s.contains("FAIL TestA"),
        "{s}"
    );
}

/// Round 8: result files arrive only at the end, so a hung xUnit/JUnit run
/// named no test; the tests of the last passing run that never reported
/// are the suspects.
#[test]
fn a_hung_run_without_live_tests_names_the_suspects() {
    let d = workspace("suspects");
    std::fs::write(
        d.join("prova.toml"),
        concat!(
            "[suite.j]\nrun = \"[ -f hang ] && exec sleep 30; mkdir -p build/test-results; ",
            "printf '<testsuite name=\\\"s\\\"><testcase classname=\\\"s\\\" name=\\\"a\\\"/><testcase classname=\\\"s\\\" name=\\\"hangs\\\"/></testsuite>' ",
            "> build/test-results/junit.xml\"\n",
        ),
    )
    .unwrap();
    let first = text(&prova(&d, &["run", "j"]));
    assert!(first.contains("PASSED"), "{first}");
    std::fs::remove_dir_all(d.join("build")).unwrap();
    std::fs::write(d.join("hang"), "").unwrap();
    let id = text(&prova(&d, &["run", "j", "--detach", "--stall-after", "1s"]))
        .trim()
        .to_owned();
    let w = text(&prova(
        &d,
        &["wait", &id, "--until", "stall", "--max-wait", "15s"],
    ));
    assert!(
        w.contains("not reported yet") && w.contains("s::hangs"),
        "{w}"
    );
    assert!(prova(&d, &["cancel", &id]).status.success());
    let s = text(&prova(
        &d,
        &["wait", &id, "--until", "done", "--max-wait", "10s"],
    ));
    assert!(
        s.contains("never reported before the run was cut short") && s.contains("s::hangs"),
        "{s}"
    );
}

/// Round 9: `#[ignore]` on a test that passed in the last passing run gave
/// a clean pass; a skipped test did not run.
#[test]
fn a_test_skipped_since_the_last_pass_makes_the_pass_unverified() {
    let d = workspace("ignored");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.t]\nrun = \"b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; echo 'test calc::a ... ok'; if [ -f ign ]; then echo 'test calc::b ... ignored'; else echo 'test calc::b ... ok'; fi\"\n",
    )
    .unwrap();
    assert!(text(&prova(&d, &["run", "t"])).contains("PASSED  "));
    std::fs::write(d.join("ign"), "").unwrap();
    let out = text(&prova(&d, &["run", "t"]));
    assert!(
        out.contains("PASSED (unverified") && out.contains("calc::b"),
        "{out}"
    );
}

/// A deleted or renamed test is reported once, not on every run after:
/// the run that reported it becomes the reference (production check:
/// a renamed test made every later run unverified).
#[test]
fn a_removed_test_is_reported_once() {
    let d = workspace("removed");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.t]\nrun = \"b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; echo 'test calc::a ... ok'; [ -f gone ] || echo 'test calc::b ... ok'\"\n",
    )
    .unwrap();
    assert!(text(&prova(&d, &["run", "t"])).contains("PASSED  "));
    std::fs::write(d.join("gone"), "").unwrap();
    let out = text(&prova(&d, &["run", "t"]));
    assert!(
        out.contains("PASSED (unverified") && out.contains("calc::b"),
        "{out}"
    );
    let out = text(&prova(&d, &["run", "t"]));
    assert!(out.contains("PASSED  "), "{out}");
}

/// Round 9: output that only looks like results (a Makefile that echoes
/// `test x ... ok`) passed, though the test is known to take far longer.
#[test]
fn results_faster_than_the_test_ever_ran_are_unverified() {
    let d = workspace("echoed");
    // The test reports its own 0.6 s; faked, the line comes at once.
    std::fs::write(d.join("r.sh"), "[ -f fake ] || { b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; sleep 0.6; }\necho '{\"type\":\"test\",\"event\":\"ok\",\"name\":\"calc::slow\",\"exec_time\":0.6}'\n").unwrap();
    std::fs::write(d.join("prova.toml"), "[suite.t]\nrun = \"sh r.sh\"\n").unwrap();
    for _ in 0..2 {
        assert!(text(&prova(&d, &["run", "t"])).contains("PASSED  "));
    }
    std::fs::write(d.join("fake"), "").unwrap();
    let out = text(&prova(&d, &["run", "t"]));
    assert!(
        out.contains("PASSED (unverified")
            && out.contains("did not come from running the tests now"),
        "{out}"
    );
}

/// Round 9: once flaky, always dismissed: an environment-dependent test
/// that then failed every time read "only known flaky test(s) failed".
#[test]
fn a_flaky_test_failing_three_times_in_a_row_is_failing_now() {
    let d = workspace("failingnow");
    suites(&d);
    run(&d, "make", "pass");
    run(&d, "make", "fail");
    run(&d, "make", "fail");
    run(&d, "make", "fail");
    let out = run(&d, "make", "fail");
    assert!(
        !out.contains("FLAKY") && !out.contains("only known flaky"),
        "{out}"
    );
}

/// Round 9: `prova status` showed an unverified pass as plain "passed".
#[test]
fn status_marks_an_unverified_pass() {
    let d = workspace("statusq");
    prova(&d, &["run", "--", "true"]);
    let s = text(&prova(&d, &["status"]));
    assert!(s.contains("passed?"), "{s}");
}

/// Round 10: an unverified pass (stale or echoed results) counted in the
/// test's history and made a real, deterministic failure "FLAKY".
// Needs the children's CPU time, which macOS does not give (getting started).
#[cfg(target_os = "linux")]
#[test]
fn an_unverified_pass_is_no_evidence_about_a_test() {
    let d = workspace("thinpass");
    suites(&d);
    run(&d, "make", "fail");
    run(&d, "make", "fail");
    // A pass printed without any work: unverified.
    std::fs::write(
        d.join("prova.toml"),
        "[suite.make]\nrun = \"echo 'test calc::x ... ok'\"\n",
    )
    .unwrap();
    assert!(text(&prova(&d, &["run", "make"])).contains("unverified"));
    suites(&d);
    let out = run(&d, "make", "fail");
    assert!(
        !out.contains("FLAKY") && !out.contains("only known flaky"),
        "{out}"
    );
}

/// Round 10: a coin-flip test fails 3 times in a row 1 run in 8; that is
/// still flaky, not "failing now".
#[test]
fn a_short_streak_of_a_frequent_flake_is_still_flaky() {
    let d = workspace("coinflip");
    suites(&d);
    for st in ["pass", "fail", "pass", "fail", "fail"] {
        run(&d, "make", st);
    }
    let out = run(&d, "make", "fail");
    assert!(
        out.contains("FLAKY") && !out.contains("failing now"),
        "{out}"
    );
}

/// Round 10: `@Disabled` on a test that passed before gave a clean pass in
/// an ad-hoc run (its own command had no earlier pass to compare with).
#[test]
fn a_test_skipped_now_that_passed_in_another_command_is_unverified() {
    let d = workspace("skipadhoc");
    prova(
        &d,
        &[
            "run",
            "--",
            "sh",
            "-c",
            &format!("{WORK_SH}echo 'test calc::d ... ok'"),
        ],
    );
    let o = prova(
        &d,
        &[
            "run",
            "--",
            "sh",
            "-c",
            &format!("{WORK_SH}echo 'test calc::a ... ok'; echo 'test calc::d ... ignored'"),
        ],
    );
    let out = text(&o);
    assert!(
        out.contains("unverified") && out.contains("calc::d"),
        "{out}"
    );
}

const WORK_SH: &str = "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; ";

/// Round 11: a test that fails only when a variable is set was labelled
/// FLAKY after one pass/fail pair, and "nothing else failed" hid it.
#[test]
fn a_failure_that_depends_on_the_environment_is_not_flaky() {
    let d = workspace("envdep");
    std::fs::write(
        d.join("prova.toml"),
        format!("[suite.t]\nrun = \"{WORK_SH}if [ -n \\\"$BADENV\\\" ]; then echo 'test calc::e ... FAILED'; exit 1; else echo 'test calc::e ... ok'; fi\"\n"),
    )
    .unwrap();
    prova(&d, &["run", "t"]);
    let o = Command::new(PROVA)
        .args(["run", "t"])
        .current_dir(&d)
        .env("PROVA_HOME", format!("{}.home", d.display()))
        .env("BADENV", "1")
        .output()
        .unwrap();
    let out = text(&o);
    assert!(
        out.contains("environment-dependent") && out.contains("BADENV") && !out.contains("FLAKY"),
        "{out}"
    );
}

/// Round 11: a poisoned run made a real bug "flaky" for good; `prova
/// forget` takes it out of history.
#[test]
fn a_forgotten_run_no_longer_counts() {
    let d = workspace("forget");
    suites(&d);
    run(&d, "make", "pass");
    let bad: serde_json::Value =
        serde_json::from_str(&text(&prova(&d, &["status", "last", "--json"]))).unwrap();
    run(&d, "make", "fail");
    assert!(text(&prova(&d, &["flaky"])).contains("calc::x"));
    let o = prova(
        &d,
        &[
            "forget",
            bad["run"].as_str().unwrap(),
            "--reason",
            "results were echoed",
        ],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert!(!text(&prova(&d, &["flaky"])).contains("calc::x"));
}

/// Round 12: outside git every run looked like the same code, so a fix
/// (fail, edit, pass) was labelled a flaky flip.
#[test]
fn a_fix_outside_git_is_not_flaky() {
    let d = std::env::temp_dir().join(format!("prova-hl-nogit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    suites(&d);
    run(&d, "make", "fail");
    let out = run(&d, "make", "pass");
    assert!(!out.contains("flaky"), "{out}");
    let flaky = text(&prova(&d, &["flaky"]));
    assert!(!flaky.contains("calc::x"), "{flaky}");
}

/// Round 12: the JSON history called a cancelled run "errored" while the
/// text said "cancelled".
#[test]
fn a_cancelled_run_is_cancelled_in_json_too() {
    let d = workspace("cancelled-json");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.t]\nrun = \"if [ \\\"$(cat state)\\\" = hang ]; then exec sleep 30; fi; echo 'test m::a ... ok'\"\n",
    )
    .unwrap();
    std::fs::write(d.join("state"), "hang").unwrap();
    let o = prova(&d, &["run", "t", "--detach", "--json"]);
    let id = serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["run"]
        .as_str()
        .unwrap()
        .to_owned();
    prova(&d, &["cancel", &id]);
    std::fs::write(d.join("state"), "pass").unwrap();
    let o = prova(&d, &["run", "t", "--json", "--until", "done"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["history"]["last"]["outcome"], "cancelled", "{v}");
}

/// Round 13 (Rust): an old run's labels were recomputed from today's
/// history ("failed 8x before" on a run that followed 3 failures; a test
/// called known-flaky before it ever flaked), and the run list showed
/// unverified passes as plain "passed".
#[test]
fn a_runs_labels_are_as_of_that_run() {
    let d = workspace("as-of");
    suites(&d);
    run(&d, "make", "fail");
    run(&d, "make", "fail");
    let second: serde_json::Value =
        serde_json::from_str(&text(&prova(&d, &["status", "last", "--json"]))).unwrap();
    let second = second["run"].as_str().unwrap().to_owned();
    run(&d, "make", "fail");
    run(&d, "make", "pass");
    run(&d, "make", "fail");
    let then = text(&prova(&d, &["status", &second]));
    assert!(then.contains("FAIL calc::x [failed 1x before]"), "{then}");
    assert!(!then.contains("FLAKY"), "{then}");
    // The latest pass is of a now-flaky test: unverified in the list too.
    run(&d, "make", "pass");
    let list = text(&prova(&d, &["status"]));
    assert!(
        list.lines().next().is_some_and(|l| l.contains("passed?")),
        "{list}"
    );
}

/// Round 13 (Java): five runs whose selection matched nothing (each run
/// failed, no test ran) were tallied as "every test passed every time".
#[test]
fn repeat_never_calls_runs_without_tests_green() {
    let d = workspace("repeat-empty");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.t]\nrun = \"i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; echo 'No tests matching pattern'; exit 1\"\n",
    )
    .unwrap();
    let o = prova(&d, &["run", "t", "--repeat", "2"]);
    let out = text(&o);
    assert!(!out.contains("every test passed"), "{out}");
    assert!(out.contains("2 of 2 run(s) ran no test"), "{out}");
    let progress = String::from_utf8_lossy(&o.stderr);
    assert!(progress.contains("(no test ran)"), "{progress}");
}

/// Round 13 red team: VSTest's console names NUnit/MSTest tests by bare
/// method, so two classes' `Divide` were told apart by console order; when
/// the order flipped, their histories swapped and a test failing every run
/// was called FLAKY ("nothing else failed"). The TRX's qualified names are
/// the identity.
#[test]
fn bare_console_names_take_the_result_files_identity() {
    let d = workspace("trx-identity");
    let trx = r#"<TestRun xmlns="http://microsoft.com/schemas/VisualStudio/TeamTest/2010"><TestDefinitions><UnitTest name="Divide" id="1"><TestMethod className="P.CalcTests" name="Divide"/></UnitTest><UnitTest name="Divide" id="2"><TestMethod className="P.MathTests" name="Divide"/></UnitTest></TestDefinitions><Results><UnitTestResult testId="1" testName="Divide" outcome="Failed"/><UnitTestResult testId="2" testName="Divide" outcome="Passed"/></Results></TestRun>"#;
    std::fs::write(d.join("r.trx.src"), trx).unwrap();
    let busy = "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done";
    let lines = |first: &str, second: &str| {
        format!(
            "{busy}; printf '  {first} Divide [1 ms]\\n  {second} Divide [1 ms]\\n'; mkdir -p TestResults; cp r.trx.src TestResults/r.trx; exit 1"
        )
    };
    for (a, b) in [
        ("Failed", "Passed"),
        ("Passed", "Failed"),
        ("Failed", "Passed"),
    ] {
        let out = text(&prova(&d, &["run", "--", "sh", "-c", &lines(a, b)]));
        assert!(!out.contains("FLAKY"), "{out}");
        assert!(out.contains("tests: 1 passed, 1 failed"), "{out}");
    }
    let h = text(&prova(&d, &["history", "P.CalcTests.Divide"]));
    assert!(h.contains("3 runs with a result, 3 failed"), "{h}");
}

/// Round 13 (Go): selecting a subtest ran its parent, which passed there;
/// with the full runs' failures on the same code the parent became FLAKY
/// and every later green run "proved little".
#[test]
fn a_parent_run_for_its_selected_subtest_is_not_evidence() {
    let d = workspace("subtest-parent");
    let busy = "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done";
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.t]\nrun = \"{busy}; printf '=== RUN   TestX\\\\n=== RUN   TestX/bad\\\\n    --- FAIL: TestX/bad (0.00s)\\\\n=== RUN   TestX/ok\\\\n    --- PASS: TestX/ok (0.00s)\\\\n--- FAIL: TestX (0.00s)\\\\n'; exit 1\"\nselect = \"{busy}; printf '=== RUN   TestX\\\\n=== RUN   TestX/ok\\\\n    --- PASS: TestX/ok (0.00s)\\\\n--- PASS: TestX (0.00s)\\\\n'\"\n"
        ),
    )
    .unwrap();
    text(&prova(&d, &["run", "t"]));
    text(&prova(&d, &["run", "t", "--select", "TestX/ok"]));
    let out = text(&prova(&d, &["run", "t"]));
    assert!(!out.contains("FLAKY"), "{out}");
    assert!(out.contains("FAIL TestX [failed 1x before]"), "{out}");
}
