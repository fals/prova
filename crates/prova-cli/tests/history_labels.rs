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
fn each_suite_has_its_own_history_of_the_same_test() {
    let d = workspace("shared");
    suites(&d);
    // Flaky in `make`: passes and fails on the same code.
    run(&d, "make", "pass");
    run(&d, "make", "fail");
    // In `default` it is its own test: nothing known yet (ADR 0037).
    let out = run(&d, "default", "fail");
    assert!(
        out.contains("FAIL calc::x [first time seen") && !out.contains("FLAKY"),
        "{out}"
    );
    // `history` lists one entry per suite.
    let h = text(&prova(&d, &["history", "calc::x"]));
    assert!(
        h.contains("calc::x  (suite default)") && h.contains("calc::x  (suite make)"),
        "{h}"
    );
    assert!(
        h.contains("2 runs with a result, 1 failed")
            && h.contains("1 runs with a result, 1 failed"),
        "{h}"
    );
}

/// Review of PR 126 (C), ADR 0037: `history --json` prints one JSON array,
/// one entry per suite, also for a single suite (it printed one object per
/// suite per line).
#[test]
fn history_json_is_one_array_with_an_entry_per_suite() {
    let d = workspace("histjson");
    suites(&d);
    run(&d, "make", "pass");
    let one = text(&prova(&d, &["history", "calc::x", "--json"]));
    let one: serde_json::Value = serde_json::from_str(&one).unwrap();
    assert_eq!(one.as_array().map(Vec::len), Some(1), "{one}");
    assert_eq!(one[0]["suite"], "make", "{one}");
    run(&d, "default", "fail");
    let two = text(&prova(&d, &["history", "calc::x", "--json"]));
    let two: serde_json::Value = serde_json::from_str(&two).unwrap();
    let mut suites: Vec<&str> = two
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["suite"].as_str().unwrap())
        .collect();
    suites.sort_unstable();
    assert_eq!(suites, ["default", "make"], "{two}");
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

/// Round 5, ADR 0037: `go test` text names no package; within one suite its
/// results are still the same tests as the full streams'. Another suite's
/// pass is no evidence. (Quiet `cargo test` names no binary: it is no go.)
#[test]
fn text_runs_join_the_full_history_of_their_suite() {
    let d = workspace("join");
    std::fs::write(
        d.join("prova.toml"),
        concat!(
            "[suite.rust]\nrun = \"if [ ! -f quiet ]; then echo '     Running unittests src/lib.rs (target/debug/deps/calc-0a1b)'; fi; sh r.sh\"\n",
            "[suite.rust_other]\nrun = \"sh r.sh\"\n",
            "[suite.go]\nrun = \"if [ -f text ]; then echo '--- FAIL: TestDiv (0.00s)'; printf 'FAIL\\\\texample.com/calc\\\\t0.01s\\\\n'; exit 1; fi; sh g.sh\"\n",
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
    prova(&d, &["run", "rust"]);
    std::fs::write(d.join("quiet"), "").unwrap();
    std::fs::write(d.join("state"), "fail").unwrap();
    let out = text(&prova(&d, &["run", "rust"]));
    assert!(
        out.contains("FAIL tests::x [first time seen"),
        "a result of unknown binary shares no history with a known one: {out}"
    );
    let out = text(&prova(&d, &["run", "rust_other"]));
    assert!(
        out.contains("FAIL tests::x [first time seen"),
        "another suite's passes are no history: {out}"
    );
    prova(&d, &["run", "go"]);
    std::fs::write(d.join("text"), "").unwrap();
    let out = text(&prova(&d, &["run", "go"]));
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

/// Review of PR 126 (C), ADR 0037: quiet `cargo test` names no binary, so
/// its `tests::x` is a test of unknown place. The suite's one known place
/// (`calc`) is not it: another crate's quiet failure was "failed 1x before".
#[test]
fn a_quiet_cargo_test_shares_no_history_with_a_known_binary() {
    let d = workspace("quietcargo");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.rust]\nrun = \"if [ ! -f quiet ]; then echo '     Running unittests src/lib.rs (target/debug/deps/calc-0a1b)'; fi; b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; echo 'test tests::x ... FAILED'; exit 1\"\n",
    )
    .unwrap();
    let out = text(&prova(&d, &["run", "rust"]));
    assert!(out.contains("FAIL tests::x"), "{out}");
    std::fs::write(d.join("quiet"), "").unwrap();
    let out = text(&prova(&d, &["run", "rust"]));
    assert!(
        out.contains("FAIL tests::x [first time seen") && !out.contains("failed 1x before"),
        "{out}"
    );
    // Quiet runs are of one place with each other.
    let out = text(&prova(&d, &["run", "rust"]));
    assert!(out.contains("failed 1x before"), "{out}");
}

/// Review of PR 126 (C): the quiet result and the known crate's `tests::x`
/// are two tests; `history` shows both, not the most recent of the two.
#[test]
fn history_shows_a_test_of_unknown_place_beside_a_known_one() {
    let d = workspace("quietcargohist");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.rust]\nrun = \"if [ ! -f quiet ]; then echo '     Running unittests src/lib.rs (target/debug/deps/calc-0a1b)'; fi; b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; echo 'test tests::x ... FAILED'; exit 1\"\n",
    )
    .unwrap();
    prova(&d, &["run", "rust"]);
    std::fs::write(d.join("quiet"), "").unwrap();
    prova(&d, &["run", "rust"]);
    let h = text(&prova(&d, &["history", "tests::x", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&h).unwrap();
    assert_eq!(v.as_array().map(Vec::len), Some(2), "{h}");
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

/// ADR 0036, 0037: a test that fails every time one way of running it and
/// passes another way, on the same code, is not flaky (acceptance
/// 2026-11-26: it was FLAKY and "nothing else failed"). Its pass elsewhere
/// is no history of this suite: the failure is first time seen.
#[test]
fn a_pass_in_another_suite_is_not_flaky_evidence() {
    let d = workspace("flakyacross");
    suites(&d);
    run(&d, "make", "pass");
    run(&d, "make", "pass");
    let out = run(&d, "default", "fail");
    assert!(
        out.contains("FAIL calc::x [first time seen") && !out.contains("FLAKY"),
        "{out}"
    );
    assert!(!out.contains("only known flaky"), "{out}");
    let f = text(&prova(&d, &["flaky"]));
    assert!(!f.contains("calc::x"), "{f}");
    // The same flip within one suite is flaky, and `prova flaky` lists it
    // whichever suite flipped (round 6).
    run(&d, "default", "pass");
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

/// Round 17: `prova history TestHang` said "0 runs with a result" though the
/// test was the one running when two runs were cut short. History names the
/// cut-short runs in which the test was running (ADR 0031 §4).
#[test]
fn history_names_the_cut_short_runs_in_which_a_test_was_running() {
    let d = workspace("history-cut-short");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.go]\nrun = \"echo '=== RUN   TestHang'; sleep 30 & wait\"\n",
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
    prova(&d, &["wait", &id, "--until", "done", "--max-wait", "10s"]);
    let h = text(&prova(&d, &["history", "TestHang"]));
    assert!(
        h.contains("cut short") && h.contains(&id[..8.min(id.len())]),
        "{h}"
    );
    let j = text(&prova(&d, &["history", "TestHang", "--json"]));
    let j: serde_json::Value = serde_json::from_str(&j).unwrap();
    assert_eq!(j[0]["cut_short"][0]["run"], id.as_str(), "{j}");
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

/// Round 18: with nothing reported, the suspects are every test the suite
/// has; the note says the hung test cannot be told instead of presenting
/// the whole suite as a lead. Medium: the stall note of a real run.
#[test]
fn a_whole_suite_of_suspects_says_it_cannot_tell() {
    let d = workspace("suspects-whole");
    std::fs::write(
        d.join("prova.toml"),
        concat!(
            "[suite.j]\nrun = \"[ -f hang ] && exec sleep 30; mkdir -p build/test-results; ",
            "printf '<testsuite name=\\\"s\\\"><testcase classname=\\\"s\\\" name=\\\"a\\\"/><testcase classname=\\\"s\\\" name=\\\"hangs\\\"/></testsuite>' ",
            "> build/test-results/junit.xml\"\n",
        ),
    )
    .unwrap();
    assert!(text(&prova(&d, &["run", "j"])).contains("PASSED"));
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
        w.contains("cannot be told")
            && w.contains("before any test reported")
            && !w.contains("likely among them"),
        "{w}"
    );
    prova(&d, &["cancel", &id]);
    prova(&d, &["wait", &id, "--until", "done", "--max-wait", "10s"]);
}

/// Round 18: a stalled `make` run names the project's suite that runs the
/// runner directly, which names the hung test.
#[test]
fn a_stalled_make_run_names_the_direct_suite() {
    let d = workspace("stall-make-direct");
    std::fs::write(d.join("Makefile"), "test:\n\tsleep 30\n").unwrap();
    std::fs::write(
        d.join("prova.toml"),
        "[suite.mk]\nrun = \"make test\"\n[suite.direct]\nrun = \"go test -json ./...\"\n",
    )
    .unwrap();
    let id = text(&prova(
        &d,
        &["run", "mk", "--detach", "--stall-after", "1s"],
    ))
    .trim()
    .to_owned();
    let w = text(&prova(
        &d,
        &["wait", &id, "--until", "stall", "--max-wait", "15s"],
    ));
    assert!(
        w.contains("prova run direct --until first-failure")
            && w.contains("if it runs these tests"),
        "{w}"
    );
    prova(&d, &["cancel", &id]);
    prova(&d, &["wait", &id, "--until", "done", "--max-wait", "10s"]);
}

/// The make hint reads the command's own words, not its shell-quoted
/// display: a `make` whose path holds a space (`'./my tools/make' test`
/// once displayed) is still make (review of PR 118). Why medium: a real
/// stalled argv run and its summary.
#[test]
fn a_stalled_make_with_a_spaced_path_names_the_direct_suite() {
    use std::os::unix::fs::PermissionsExt;
    let d = workspace("stall-make-space");
    let tools = d.join("my tools");
    std::fs::create_dir_all(&tools).unwrap();
    std::fs::write(tools.join("make"), "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(tools.join("make"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        d.join("prova.toml"),
        "[suite.direct]\nrun = \"go test -json ./...\"\n",
    )
    .unwrap();
    let id = text(&prova(
        &d,
        &[
            "run",
            "--detach",
            "--stall-after",
            "1s",
            "--",
            "./my tools/make",
            "test",
        ],
    ))
    .trim()
    .to_owned();
    let w = text(&prova(
        &d,
        &["wait", &id, "--until", "stall", "--max-wait", "15s"],
    ));
    assert!(w.contains("prova run direct --until first-failure"), "{w}");
    prova(&d, &["cancel", &id]);
    prova(&d, &["wait", &id, "--until", "done", "--max-wait", "10s"]);
}

/// Round 16: a list of suspects cut at five ended in "..." and hid the hung
/// test. A list cut short says how many it left out.
#[test]
fn a_long_suspect_list_says_how_many_it_left_out() {
    let d = workspace("suspects-many");
    let cases: String = (1..=12)
        .map(|i| format!("<testcase classname=\\\"s\\\" name=\\\"t{i:02}\\\"/>"))
        .collect();
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.j]\nrun = \"[ -f hang ] && exec sleep 30; mkdir -p build/test-results; printf '<testsuite name=\\\"s\\\">{cases}</testsuite>' > build/test-results/junit.xml\"\n"
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
        w.contains("12 of the last finished run") && w.contains("first 8 of 12; 4 more not listed"),
        "{w}"
    );
    prova(&d, &["cancel", &id]);
    prova(&d, &["wait", &id, "--until", "done", "--max-wait", "10s"]);
}

/// Round 17: the "not reported yet" list was alphabetical and left out the
/// tests the last run skipped (`TestHang`, skipped unless HANG=1), so the
/// hung test was hidden. The list follows the last run's order, counts its
/// skipped tests, and is whole in `status --json` (ADR 0031 §4).
#[test]
fn the_suspects_follow_the_last_runs_order_and_include_its_skipped_tests() {
    let d = workspace("suspects-order");
    std::fs::write(
        d.join("prova.toml"),
        concat!(
            "[suite.j]\nrun = \"[ -f hang ] && exec sleep 30; mkdir -p build/test-results; ",
            "printf '<testsuite name=\\\"s\\\"><testcase classname=\\\"s\\\" name=\\\"zeta\\\"/><testcase classname=\\\"s\\\" name=\\\"alpha\\\"/><testcase classname=\\\"s\\\" name=\\\"hangs\\\"><skipped/></testcase></testsuite>' ",
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
    let at = |n: &str| w.find(n).unwrap_or(usize::MAX);
    assert!(
        at("s::zeta") < at("s::alpha") && at("s::alpha") < at("s::hangs"),
        "{w}"
    );
    let j: serde_json::Value = serde_json::from_str(&text(&prova(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "stall",
            "--max-wait",
            "5s",
            "--json",
        ],
    )))
    .unwrap();
    assert_eq!(
        j["not_reported"],
        serde_json::json!(["s::zeta", "s::alpha", "s::hangs"]),
        "{j}"
    );
    prova(&d, &["cancel", &id]);
    prova(&d, &["wait", &id, "--until", "done", "--max-wait", "10s"]);
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
    // The test reports its own 2 s; faked, the line comes at once.
    std::fs::write(d.join("r.sh"), "[ -f fake ] || { b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; sleep 2; }\necho '{\"type\":\"test\",\"event\":\"ok\",\"name\":\"calc::slow\",\"exec_time\":2.0}'\n").unwrap();
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
/// an ad-hoc run (the same command had an earlier pass to compare with).
#[test]
fn a_test_skipped_now_that_passed_in_the_same_command_is_unverified() {
    let d = workspace("skipadhoc");
    // The same command twice: an ad hoc command is its own suite.
    let cmd = format!(
        "{WORK_SH}echo 'test calc::a ... ok'; if [ -f state ]; then echo 'test calc::d ... ignored'; else echo 'test calc::d ... ok'; fi"
    );
    prova(&d, &["run", "--", "sh", "-c", &cmd]);
    std::fs::write(d.join("state"), "").unwrap();
    let o = prova(&d, &["run", "--", "sh", "-c", &cmd]);
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
    // Two passes: one is too little history to call a failure
    // environment-dependent (ADR 0035 section 2).
    prova(&d, &["run", "t"]);
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
    // One command (history is per suite); the file `state` swaps the order.
    let cmd = format!(
        "{busy}; if [ -f state ]; then printf '  Passed Divide [1 ms]\\n  Failed Divide [1 ms]\\n'; else printf '  Failed Divide [1 ms]\\n  Passed Divide [1 ms]\\n'; fi; mkdir -p TestResults; cp r.trx.src TestResults/r.trx; exit 1"
    );
    for flip in [false, true, false] {
        let _ = std::fs::remove_file(d.join("state"));
        if flip {
            std::fs::write(d.join("state"), "").unwrap();
        }
        let out = text(&prova(&d, &["run", "--", "sh", "-c", &cmd]));
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

/// Round 13: `next: done` under an unverified pass, and a `next:` that
/// suggested `--select` to a suite without a `select` template (it failed
/// with a usage error).
#[test]
fn next_lines_are_honest_and_runnable() {
    let d = workspace("next-lines");
    suites(&d);
    run(&d, "make", "pass");
    run(&d, "make", "fail");
    let out = run(&d, "make", "pass");
    assert!(
        out.contains("next: done, but this pass is unverified"),
        "{out}"
    );
    let out = run(&d, "make", "fail");
    assert!(out.contains("`prova run make --repeat 5`"), "{out}");
    assert!(!out.contains("--select"), "{out}");
}

/// Round 14: every pass of a suite with a flaky test passing relied on it,
/// and checking each for "last green" summarized the earlier ones in turn,
/// exponential in the number of passes (`prova status` used a second of CPU
/// after 15 runs). The history then said "none of them passed".
#[test]
fn passes_with_a_flaky_test_are_cheap_to_check_and_named_as_passes() {
    let d = workspace("flakychain");
    suites(&d);
    run(&d, "make", "fail");
    for _ in 0..16 {
        run(&d, "make", "pass");
    }
    run(&d, "make", "fail");
    let started = std::time::Instant::now();
    let status = text(&prova(&d, &["status", "last"]));
    let list = text(&prova(&d, &["status"]));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
    assert!(
        !status.contains("none of them passed;")
            && status.contains("none of them passed verified (16 passed with warnings"),
        "{status}"
    );
    assert!(list.contains("passed?"), "{list}");
}

/// Round 14: a test that flipped on one code in two environments was
/// "both passed and failed on 2 code version(s)".
#[test]
fn flips_count_code_versions_not_environments() {
    let d = workspace("flipenv");
    suites(&d);
    let with_env = |state: &str| {
        std::fs::write(d.join("state"), state).unwrap();
        Command::new(PROVA)
            .args(["run", "make"])
            .current_dir(&d)
            .env("PROVA_HOME", format!("{}.home", d.display()))
            .env("BADENV", "1")
            .output()
            .unwrap()
    };
    run(&d, "make", "pass");
    run(&d, "make", "fail");
    with_env("pass");
    with_env("fail");
    let h = text(&prova(&d, &["history", "calc::x"]));
    assert!(
        h.contains("both passed and failed on 1 code version(s)"),
        "{h}"
    );
}

/// Round 14: a run that failed before any test (no SDK installed) counted
/// against the code in the next run's history, and nothing said how to
/// take it out.
#[test]
fn a_failure_with_no_test_is_named_and_can_be_forgotten() {
    let d = workspace("notest");
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.make]\nrun = {:?}\n",
            format!(
                "{WORK_SH}if [ \"$(cat state)\" = env ]; then echo 'No .NET SDKs were found.'; exit 155; fi; {SCRIPT}"
            )
        ),
    )
    .unwrap();
    let out = run(&d, "make", "env");
    assert!(
        out.contains("no test result was seen") && out.contains("prova forget"),
        "{out}"
    );
    let out = run(&d, "make", "fail");
    assert!(out.contains("failed (no test seen)"), "{out}");
}

/// Round 15: an ambiguous name listed each test once per suite ("3 tests
/// match"), as if names that cannot be told apart.
#[test]
fn ambiguous_history_lists_each_name_once() {
    let d = workspace("ambig");
    let both = format!("{WORK_SH}echo 'test calc::x ... ok'; echo 'test calc::xy ... ok'");
    std::fs::write(
        d.join("prova.toml"),
        format!("[suite.a]\nrun = {both:?}\n\n[suite.b]\nrun = {both:?}\n"),
    )
    .unwrap();
    prova(&d, &["run", "a"]);
    prova(&d, &["run", "b"]);
    let out = text(&prova(&d, &["history", "calc::"]));
    assert!(
        out.contains("2 tests match") && out.contains("calc::xy  (suite a, b)"),
        "{out}"
    );
}

/// Round 15: one earlier pass and three failures was "failing now: more
/// failures in a row than its flakiness explains", then it passed again.
/// A streak needs enough history behind it to be called a break.
#[test]
fn a_thin_history_is_not_failing_now() {
    let d = workspace("thinfailingnow");
    suites(&d);
    run(&d, "make", "pass");
    run(&d, "make", "fail");
    run(&d, "make", "fail");
    let out = run(&d, "make", "fail");
    assert!(!out.contains("failing now"), "{out}");
}

/// Round 16: quiet tests that all passed, then a failing lint step: "no
/// test result was seen" with a hint to forget a real code failure.
#[test]
fn a_step_failing_after_quiet_tests_is_not_called_testless() {
    let d = workspace("quietlint");
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.make]\nrun = {:?}\n",
            format!(
                "{WORK_SH}echo '.......'; echo '7 passed in 0.10s'; echo \"*** Error compiling './broken.py'...\"; echo 'SyntaxError: invalid syntax'; exit 1"
            )
        ),
    )
    .unwrap();
    let out = text(&prova(&d, &["run", "make"]));
    assert!(
        out.contains("FAILED") && !out.contains("prova forget"),
        "{out}"
    );
    let out = text(&prova(&d, &["run", "make", "--repeat", "2"]));
    assert!(!out.contains("(no test ran)"), "{out}");
}

/// Round 16: skipped tests were counted but never named.
#[test]
fn skipped_tests_are_named() {
    let d = workspace("skipnames");
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.make]\nrun = {:?}\n",
            format!("{WORK_SH}echo 'test calc::a ... ok'; echo 'test calc::later ... ignored'")
        ),
    )
    .unwrap();
    let out = text(&prova(&d, &["run", "make"]));
    assert!(out.contains("skipped: calc::later"), "{out}");
}

/// Round 16: a selection's runner reports the tests it filtered out as
/// skipped, read as disabled tests.
#[test]
fn a_selection_says_skipped_tests_may_be_unselected() {
    let d = workspace("selskip");
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.make]\nrun = \"true\"\nselect = {:?}\n",
            format!("{WORK_SH}echo 'test calc::a ... ok'; echo 'test calc::b ... ignored'")
        ),
    )
    .unwrap();
    let out = text(&prova(&d, &["run", "make", "--select", "calc::a"]));
    assert!(out.contains("1 skipped (or not selected)"), "{out}");
}

/// Round 16: a failing run in which a panic kept two tests from running
/// did not say they were missing.
#[test]
fn a_failed_run_names_tests_that_did_not_run() {
    let d = workspace("failmissing");
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.make]\nrun = {:?}\n",
            format!(
                "{WORK_SH}if [ \"$(cat state)\" = pass ]; then echo 'test calc::a ... ok'; echo 'test calc::b ... ok'; echo 'test calc::c ... ok'; else echo 'test calc::a ... FAILED'; exit 101; fi"
            )
        ),
    )
    .unwrap();
    run(&d, "make", "pass");
    let out = run(&d, "make", "fail");
    assert!(
        out.contains("2 test(s) from the last passing run") && out.contains("calc::b"),
        "{out}"
    );
}

/// Round 16: `--select tests.test_q::test_flaky` became `-k test_flaky`,
/// which ran only `test_flaky_slow`; the run passed.
#[test]
fn a_named_selection_must_run_the_test_it_names() {
    let d = workspace("selnamed");
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.py]\nrun = \"true\"\nselect = {:?}\n",
            format!(
                "{WORK_SH}: pytest -k {{filter}}; echo 'collected 4 items / 3 deselected / 1 selected'; echo 'tests/test_q.py::test_flaky_slow PASSED'; echo '1 passed, 3 deselected in 0.01s'"
            )
        ),
    )
    .unwrap();
    let o = prova(&d, &["run", "py", "--select", "tests.test_q::test_flaky"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains(
            "that test did not run; the filter matched other tests (tests.test_q::test_flaky_slow)"
        ),
        "{out}"
    );
}

/// Review of PR 126 (C): `slowest` and `flaky` listed a name once per suite
/// without saying which suite each row was of (`sizes` did).
#[test]
fn flaky_names_its_suite_when_a_name_is_flaky_in_several() {
    let d = workspace("flakysuites");
    suites(&d);
    for suite in ["make", "default"] {
        run(&d, suite, "pass");
        run(&d, suite, "fail");
    }
    let f = text(&prova(&d, &["flaky"]));
    assert!(
        f.contains("calc::x  (suite make)") && f.contains("calc::x  (suite default)"),
        "{f}"
    );
    run(&d, "make", "pass");
    run(&d, "default", "pass");
    let f = text(&prova(&d, &["flaky", "--suite", "make"]));
    assert!(f.contains("calc::x") && !f.contains("(suite"), "{f}");
}

/// Why medium: durations are only believed when the run really took that
/// long (ADR 0022), so each script sleeps.
#[test]
fn slowest_names_its_suite_when_a_name_is_in_several() {
    let d = workspace("slowsuites");
    let go = |n: &str| {
        format!(
            "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; sleep 1.7; echo '--- PASS: TestSlow ({n}s)'; echo 'PASS'; printf 'ok  \\texample.com/calc\\t0.01s\\n'"
        )
    };
    std::fs::write(
        d.join("prova.toml"),
        format!(
            "[suite.one]\nrun = {:?}\n\n[suite.two]\nrun = {:?}\n",
            go("1.50"),
            go("1.60")
        ),
    )
    .unwrap();
    prova(&d, &["run", "one"]);
    prova(&d, &["run", "two"]);
    let s = text(&prova(&d, &["slowest"]));
    assert!(
        s.contains("calc::TestSlow  (suite one)") && s.contains("calc::TestSlow  (suite two)"),
        "{s}"
    );
}

/// A test flaky in suite `make` only (it passed and failed on the same code).
fn flaky_in_make_only(name: &str) -> PathBuf {
    let d = workspace(name);
    suites(&d);
    run(&d, "make", "pass");
    run(&d, "make", "fail");
    let f = text(&prova(&d, &["flaky"]));
    assert!(f.contains("calc::x"), "{f}");
    d
}

/// Review of PR 126 (C), ADR 0037: `prova diff` called a fix "known flaky"
/// from the flakiness another suite recorded for the same name.
#[test]
fn a_diff_does_not_call_a_fix_flaky_from_another_suites_history() {
    let d = flaky_in_make_only("flakydiff");
    run(&d, "default", "fail");
    let failed = text(&prova(&d, &["status", "last", "--json"]));
    let failed: serde_json::Value = serde_json::from_str(&failed).unwrap();
    // A fix: other code, so the flip is no flake of this suite.
    std::fs::write(d.join("fix.txt"), "fix").unwrap();
    run(&d, "default", "pass");
    let diff = text(&prova(
        &d,
        &["diff", failed["run"].as_str().unwrap(), "last"],
    ));
    assert!(
        diff.contains("fixed") && !diff.contains("known flaky"),
        "{diff}"
    );
}

/// Review of PR 126 (C): a pass was "known flaky passed" (unverified) because
/// another suite knew the test as flaky.
#[test]
fn a_pass_is_not_called_a_flaky_pass_from_another_suites_history() {
    let d = flaky_in_make_only("flakypassx");
    let out = run(&d, "default", "pass");
    assert!(
        !out.contains("known flaky") && !out.contains("unverified"),
        "{out}"
    );
}

/// Review of PR 126 (C): the `--repeat` note "history knows it as flaky"
/// came from another suite's history.
#[test]
fn a_repeat_does_not_call_a_test_flaky_from_another_suites_history() {
    let d = flaky_in_make_only("flakyrepeat");
    std::fs::write(d.join("state"), "pass").unwrap();
    let out = text(&prova(&d, &["run", "default", "--repeat", "2"]));
    assert!(out.contains("2 run(s)"), "{out}");
    assert!(!out.contains("history knows it as flaky"), "{out}");
}

/// Release candidate: `history` showed an ad hoc suite as `cmd:38dcb90d`
/// only. It also says the command the name stands for.
#[test]
fn history_of_an_ad_hoc_suite_shows_its_command() {
    let d = workspace("adhoc-command");
    let cmd = format!("{SCRIPT} # a command with a long tail {}", "x".repeat(120));
    std::fs::write(d.join("state"), "pass").unwrap();
    prova(&d, &["run", "--", "sh", "-c", &cmd]);
    let h = text(&prova(&d, &["history", "calc::x"]));
    assert!(
        h.contains("(suite cmd:") && h.contains("sh -c") && h.contains("..."),
        "{h}"
    );
    assert!(!h.contains(&"x".repeat(100)), "shortened: {h}");
    let j: serde_json::Value =
        serde_json::from_str(&text(&prova(&d, &["history", "calc::x", "--json"]))).unwrap();
    assert!(
        j[0]["suite_command"]
            .as_str()
            .unwrap()
            .contains(&"x".repeat(120)),
        "{j}"
    );
    // A named suite stands for itself.
    suites(&d);
    run(&d, "make", "pass");
    let j: serde_json::Value =
        serde_json::from_str(&text(&prova(&d, &["history", "calc::x", "--json"]))).unwrap();
    assert!(
        j.as_array()
            .unwrap()
            .iter()
            .any(|s| s["suite"] == "make" && s.get("suite_command").is_none()),
        "{j}"
    );
}
