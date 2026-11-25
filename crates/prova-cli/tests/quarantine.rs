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

/// A script named like the test runner stands in for it and prints what the
/// scripted command printed.
fn runner(d: &Path, key: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let dir = d.join("rc").join(key);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cargo");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!("./rc/{key}/cargo test")
}

/// The text of a `{:?}`-printed string, back.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('u') => {
                let hex: String = it.by_ref().skip(1).take_while(|c| *c != '}').collect();
                out.push(char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap());
            }
            Some(c) => out.push(c),
            None => {}
        }
    }
    out
}

/// Writes `prova.toml`, with each suite's scripted `run` command replaced by
/// a one-runner-call command that prints the same.
fn put(d: &Path, toml: String) {
    let mut out = String::new();
    for (i, line) in toml.lines().enumerate() {
        match line
            .strip_prefix("run = \"")
            .and_then(|r| r.strip_suffix('"'))
        {
            Some(body) => {
                let cmd = runner(d, &format!("s{i}"), &unescape(body));
                out.push_str(&format!("run = {cmd:?}\n"));
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    std::fs::write(d.join("prova.toml"), out).unwrap();
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
    put(
        d,
        format!(
            "[suite.make]\nrun = {:?}\n\n[suite.stable]\nrun = {:?}\n",
            full(),
            without_x()
        ),
    );
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

/// ADR 0034: a quarantined test that passes is still a known flaky test
/// passing, which proves little: the pass is unverified, as without the
/// quarantine; the quarantine is listed.
#[test]
fn a_quarantined_test_that_passes_is_still_a_weak_pass() {
    let d = workspace("passes");
    flaky_history(&d);
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    let out = text(&run(&d, "make", "pass"));
    assert!(
        out.contains("PASSED (unverified") && out.contains("known flaky"),
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
    put(
        &d,
        format!(
            "[suite.make]\nrun = {:?}\n",
            format!(
                "{BUSY} if [ \"$(cat state)\" = pass ]; then echo 'test calc::x ... ok'; echo 'test calc::z ... ok'; else echo 'test calc::x ... FAILED'; echo 'test calc::z ... FAILED'; exit 1; fi"
            )
        ),
    );
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
/// pass, because the new suite had no history of its own. ADR 0034: a
/// quarantine does not excuse it; the warning stays.
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
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    let out = text(&run(&d, "stable", "pass"));
    assert!(
        out.contains("PASSED (unverified") && out.contains("did not run: calc::x"),
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
    put(
        &d,
        format!(
            "[suite.make]\nrun = {:?}\n",
            format!(
                "{BUSY} sleep 1.7; echo '--- PASS: TestSlow (1.50s)'; echo '--- PASS: TestFast (0.01s)'"
            )
        ),
    );
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

/// Round 14: a bare name seen once (gotestsum's text) and the same test
/// qualified (its json stream) were two tests; `add` set aside the stale
/// bare one, and the real failure still failed the run.
#[test]
fn a_bare_name_with_a_qualified_twin_is_ambiguous() {
    let d = workspace("twin");
    flaky_history(&d);
    put(
        &d,
        format!(
            "[suite.make]\nrun = {:?}\n\n[suite.bare]\nrun = {:?}\n",
            full(),
            format!("{BUSY} echo 'test x ... FAILED'; exit 1")
        ),
    );
    prova(&d, &["run", "bare"]);
    let o = prova(&d, &["quarantine", "add", "x", "--reason", "r"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(2), "{out}");
    assert!(out.contains("also known qualified, as calc::x"), "{out}");
    let o = prova(&d, &["quarantine", "add", "calc::x", "--reason", "r"]);
    let out = text(&o);
    assert!(out.contains("this machine's prova store"), "{out}");
}

/// Round 14 and ADR 0034: a run where the quarantined test fails is a
/// failed run in the run list and in `--repeat`, and says the failure is
/// the quarantined one.
#[test]
fn a_quarantined_failure_is_a_failed_run_everywhere() {
    let d = workspace("marked");
    flaky_history(&d);
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    std::fs::write(d.join("state"), "fail").unwrap();
    let o = prova(&d, &["run", "make", "--repeat", "2"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("FAIL calc::x (quarantined): failed every time"),
        "{out}"
    );
    let list = text(&prova(&d, &["status"]));
    assert!(!list.contains("passed*"), "{list}");
    let last = text(&prova(&d, &["status", "last"]));
    assert!(
        last.contains("FAILED") && last.contains("every failing test is quarantined: calc::x"),
        "{last}"
    );
    put(
        &d,
        format!(
            "[suite.only]\nrun = {:?}\n",
            format!("{BUSY} echo 'test calc::x ... FAILED'; exit 101")
        ),
    );
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
    // Every test skipped: a pass that proved little on every platform (the
    // no-CPU check needs Linux's child CPU times).
    let idle = format!("{BUSY} echo 'test calc::y ... ignored'");
    put(
        &d,
        format!("[suite.quiet]\nrun = {quiet:?}\n\n[suite.idle]\nrun = {idle:?}\n"),
    );
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

/// Round 15: every --select run listed the quarantined test "not run".
#[test]
fn a_selection_that_leaves_out_a_quarantined_test_says_nothing_of_it() {
    let d = workspace("lastgreen");
    flaky_history(&d);
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    put(
        &d,
        format!(
            "[suite.make]\nrun = {:?}\nselect = {:?}\n",
            full(),
            format!("{BUSY} echo 'test calc::y ... ok'")
        ),
    );
    let out = text(&prova(&d, &["run", "make", "--select", "calc::y"]));
    assert!(!out.contains("quarantined:"), "{out}");
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
    put(
        &d,
        format!("[suite.all]\nrun = {all:?}\n\n[suite.narrow]\nrun = {narrow:?}\n"),
    );
    run(&d, "all", "pass");
    run(&d, "all", "fail");
    let out = text(&run(&d, "narrow", "pass"));
    assert!(!out.contains("did not run"), "{out}");
}

/// Round 16: two crates' `tests::flaky` shared one name; a quarantine of
/// crate calc's covered crate geo's real failure. ADR 0034: the run fails
/// either way; only calc's own failure is called quarantined.
#[test]
fn a_quarantine_names_the_binary_when_a_name_is_shared() {
    let d = workspace("crates");
    let nx = |calc: &str, geo: &str, code: u8| {
        format!(
            "{BUSY} printf '%s\\n' '    Starting 3 tests across 2 binaries' '        PASS [   0.004s] calc tests::adds' '        {calc} [   0.004s] calc tests::flaky' '        {geo} [   0.004s] geo tests::flaky' '     Summary [   0.020s] 3 tests run: 2 passed, 1 failed'; exit {code}"
        )
    };
    put(
        &d,
        format!(
            "[suite.a]\nrun = {:?}\n\n[suite.b]\nrun = {:?}\n",
            nx("FAIL", "PASS", 100),
            nx("PASS", "FAIL", 100)
        ),
    );
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
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "calc's own: {t}");
    assert!(t.contains("every failing test is quarantined"), "{t}");
    let o = prova(&d, &["run", "b"]);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "geo's failure: {t}");
    assert!(!t.contains("every failing test is quarantined"), "{t}");
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

/// Round 16: `--repeat --json` tallied a quarantined test's failures with
/// nothing marking it quarantined.
#[test]
fn repeat_json_marks_quarantined_tests() {
    let d = workspace("repeatjson");
    flaky_history(&d);
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    std::fs::write(d.join("state"), "fail").unwrap();
    let o = prova(&d, &["run", "make", "--repeat", "2", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let x = v["tests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["test"] == "calc::x")
        .cloned()
        .unwrap();
    assert_eq!(x["quarantined"], true, "{v}");
}

/// Round 17: a quarantined test that failed in every run exited 0 with no
/// word that it is broken, not flaky.
#[test]
fn repeat_says_a_quarantined_test_that_failed_every_time_is_broken() {
    let d = workspace("repeat-broken");
    flaky_history(&d);
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    std::fs::write(d.join("state"), "fail").unwrap();
    let o = prova(&d, &["run", "make", "--repeat", "3"]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("calc::x (quarantined): failed every time")
            && out.contains("broken, not flaky"),
        "{out}"
    );
}

/// Round 17: runs refused because only a quarantined test failed were
/// tallied "with no failing test shown", right under the FLAKY line that
/// showed that failing test.
#[test]
fn repeat_names_what_failed_when_only_a_quarantined_test_did() {
    let d = workspace("repeat-refused");
    let body = format!(
        "{BUSY} n=$(cat n 2>/dev/null || echo 0); n=$((n+1)); echo $n > n; \
         if [ $((n % 2)) -eq 1 ]; then echo 'test calc::x ... FAILED'; exit 101; \
         else echo 'test calc::x ... ok'; fi"
    );
    put(&d, format!("[suite.only]\nrun = {body:?}\n"));
    prova(&d, &["run", "only"]);
    prova(&d, &["run", "only"]);
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    let out = text(&prova(&d, &["run", "only", "--repeat", "4"]));
    assert!(out.contains("FLAKY calc::x (quarantined)"), "{out}");
    assert!(
        !out.contains("no failing test shown"),
        "a quarantined test failed: {out}"
    );
}

/// Round 17: a quarantined test was "not run" when quiet output (pytest -q)
/// simply does not print passing tests.
#[test]
fn a_quarantined_test_hidden_by_quiet_output_is_not_seen_not_missing() {
    let d = workspace("quiet-not-seen");
    let body = format!(
        "{BUSY} if [ \"$(cat state)\" = quiet ]; then echo 'test calc::z ... ignored'; echo 'test result: ok. 3 passed; 0 failed; 1 ignored'; else echo 'test calc::ok ... ok'; echo 'test calc::x ... ok'; fi"
    );
    put(&d, format!("[suite.make]\nrun = {body:?}\n"));
    let first = run(&d, "make", "pass");
    assert_eq!(first.status.code(), Some(0), "{}", text(&first));
    assert!(!text(&first).contains("unverified"), "{}", text(&first));
    let o = prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let out = text(&run(&d, "make", "quiet"));
    assert!(
        out.contains("quarantined: calc::x") && out.contains("not seen (quiet output)"),
        "{out}"
    );
    assert!(!out.contains("calc::x not run"), "{out}");
}

/// Round 17: a failed command with no failing test named make's own
/// `make: *** ... Error 1` as the reason, not the line just above it.
#[test]
fn the_detail_of_a_failed_make_names_the_step_not_makes_wrap_up() {
    let d = workspace("make-detail");
    let script = format!(
        "{BUSY} echo 'lint: TODO markers are not allowed in src/main'; echo 'make: *** [Makefile:3: ci] Error 1'; exit 2"
    );
    let o = prova(&d, &["run", "--", "sh", "-c", &script]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    let detail = out
        .lines()
        .find(|l| l.starts_with("detail:"))
        .unwrap_or_default();
    assert!(
        detail.contains("TODO markers are not allowed"),
        "the culprit, not make's line: {out}"
    );
}

/// Round 17: a vet step that failed before the tests (a compiler diagnostic
/// `file.go:6:57: ...`) offered `prova forget` as if the environment had
/// failed, and the detail did not name the diagnostic.
#[test]
fn a_compiler_diagnostic_is_a_code_failure_not_an_environment_one() {
    let d = workspace("diagnostic");
    let body = format!(
        "{BUSY} if [ \"$(cat state)\" = vet ]; then echo 'vetbad.go:6:57: fmt.Sprintf format %s has arg n of wrong type int'; exit 1; fi; echo 'test calc::ok ... ok'; echo 'test calc::y ... ok'"
    );
    put(&d, format!("[suite.make]\nrun = {body:?}\n"));
    run(&d, "make", "ok");
    let o = run(&d, "make", "vet");
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(!out.contains("prova forget"), "a code failure: {out}");
    assert!(
        out.contains("the command failed first: vetbad.go:6:57"),
        "{out}"
    );
}

/// Round 17: "every test passed every time" while a test was skipped in
/// every run.
#[test]
fn repeat_does_not_call_a_skipped_test_passed() {
    let d = workspace("repeat-skipped");
    let body = format!("{BUSY} echo 'test calc::ok ... ok'; echo 'test calc::s ... ignored'");
    put(&d, format!("[suite.sk]\nrun = {body:?}\n"));
    let out = text(&prova(&d, &["run", "sk", "--repeat", "2"]));
    assert!(!out.contains("every test passed every time"), "{out}");
    assert!(
        out.contains("calc::s") && out.contains("skipped in every run"),
        "{out}"
    );
}

/// ADR 0034: a quarantine never turns a failed run green. A run whose only
/// failing test is quarantined fails (exit 1) and says that every failing
/// test is quarantined; the quarantine still shows, and history keeps the
/// failure. A medium test: the verdict comes from the CLI end to end.
#[test]
fn a_quarantine_never_turns_a_failed_run_green() {
    let d = workspace("never-green");
    flaky_history(&d);
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    let o = run(&d, "make", "fail");
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("FAILED"), "{out}");
    assert!(!out.contains("PASSED"), "{out}");
    assert!(
        out.contains("every failing test is quarantined: calc::x"),
        "{out}"
    );
    assert!(out.contains("quarantined: calc::x failed (clock"), "{out}");
    // Failures first: the quarantined failure is in the FAIL list, marked.
    assert!(out.contains("FAIL calc::x (quarantined)"), "{out}");
    // History keeps the failure: a quarantine hides nothing from it.
    let h = text(&prova(&d, &["history", "calc::x"]));
    assert!(h.contains("fail"), "{h}");
}

/// ADR 0034 (0027 §5): the one thing a quarantine changes is that a
/// first-failure wait does not wake for a failure already known; it wakes
/// for the next failure that is not quarantined. A medium test: a detached
/// run and two waits through the CLI.
#[test]
fn a_first_failure_wait_does_not_wake_for_a_quarantined_failure() {
    let d = workspace("wait-known");
    flaky_history(&d);
    prova(&d, &["quarantine", "add", "calc::x", "--reason", "clock"]);
    put(
        &d,
        format!(
            "[suite.later]\nrun = {:?}\n",
            format!(
                "{BUSY} echo 'test calc::x ... FAILED'; i=0; while [ ! -f go ] && [ $i -lt 300 ]; do sleep 0.1; i=$((i+1)); done; echo 'test calc::y ... FAILED'; echo 'test result: FAILED. 0 passed; 2 failed'; exit 101"
            )
        ),
    );
    let o = prova(&d, &["run", "later", "--detach", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let id = v["run"].as_str().unwrap().to_owned();
    let w = prova(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "first-failure",
            "--max-wait",
            "2s",
            "--json",
        ],
    );
    let s: serde_json::Value = serde_json::from_slice(&w.stdout).unwrap();
    // The wait ran out (reason `running`); it did not wake for calc::x.
    assert_eq!(s["reason"], "running", "not woken by calc::x: {s}");
    std::fs::write(d.join("go"), "").unwrap();
    let w = prova(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "first-failure",
            "--max-wait",
            "20s",
            "--json",
        ],
    );
    let s: serde_json::Value = serde_json::from_slice(&w.stdout).unwrap();
    assert!(s.to_string().contains("calc::y"), "woken by calc::y: {s}");
    let _ = prova(&d, &["cancel", &id]);
}
