//! Medium tests for ADR 0018: honest history labels. Real runs are needed:
//! labels come from the store's history across suites and run statuses.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOE: &str = env!("CARGO_BIN_EXE_toe");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("toe-hl-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn toe(dir: &Path, args: &[&str]) -> Output {
    Command::new(TOE)
        .args(args)
        .current_dir(dir)
        .env("TOE_HOME", format!("{}.home", dir.display()))
        .output()
        .expect("run toe")
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Prints one libtest line whose outcome comes from the file `state`.
const SCRIPT: &str = "if [ \"$(cat state)\" = pass ]; then echo 'test calc::x ... ok'; else echo 'test calc::x ... FAILED'; exit 1; fi";

fn suites(d: &Path) {
    std::fs::write(
        d.join("toe.toml"),
        format!("[suite.make]\nrun = {SCRIPT:?}\n\n[suite.default]\nrun = {SCRIPT:?}\n"),
    )
    .unwrap();
}

fn run(d: &Path, suite: &str, state: &str) -> String {
    std::fs::write(d.join("state"), state).unwrap();
    text(&toe(d, &["run", suite]))
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
        out.contains("FAIL calc::x [seen") && out.contains("FLAKY"),
        "{out}"
    );
    assert!(
        !out.contains("new failure") && !out.contains("first time seen"),
        "{out}"
    );
    let h = text(&toe(&d, &["history", "calc::x"]));
    assert!(h.contains("suites default, make"), "{h}");
}

#[test]
fn cancelled_runs_do_not_make_a_test_flaky() {
    let d = workspace("cancel");
    std::fs::write(
        d.join("toe.toml"),
        "[suite.go]\nrun = \"echo '=== RUN   TestA'; if [ -f hang ]; then sleep 30; fi; echo '--- PASS: TestA (0.00s)'\"\n",
    )
    .unwrap();
    assert!(toe(&d, &["run", "go"]).status.success());
    std::fs::write(d.join("hang"), "").unwrap();
    let o = toe(&d, &["run", "go", "--detach", "--stall-after", "1s"]);
    let id = text(&o).trim().to_owned();
    let w = text(&toe(
        &d,
        &["wait", &id, "--until", "stall", "--max-wait", "15s"],
    ));
    assert!(
        w.contains("STALLED") && w.contains("running now: TestA"),
        "{w}"
    );
    assert!(toe(&d, &["cancel", &id]).status.success());
    toe(&d, &["wait", &id, "--until", "done", "--max-wait", "10s"]);
    let flaky = text(&toe(&d, &["flaky"]));
    assert!(!flaky.contains("TestA"), "{flaky}");
    let h = text(&toe(&d, &["history", "TestA"]));
    assert!(h.contains("1 finished runs, 0 failed"), "{h}");
}

#[test]
fn quiet_counts_never_contradict() {
    let d = workspace("quiet");
    let o = toe(
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
        d.join("toe.toml"),
        concat!(
            "[suite.rust_full]\nrun = \"echo '     Running unittests src/lib.rs (target/debug/deps/calc-0a1b)'; sh r.sh\"\n",
            "[suite.rust_quiet]\nrun = \"sh r.sh\"\n",
            "[suite.go_json]\nrun = \"sh g.sh\"\n",
            "[suite.go_text]\nrun = \"echo '--- FAIL: TestDiv (0.00s)'; printf 'FAIL\\\\texample.com/calc\\\\t0.01s\\\\n'; exit 1\"\n",
        ),
    )
    .unwrap();
    std::fs::write(d.join("r.sh"), "if [ \"$(cat state)\" = pass ]; then echo 'test tests::x ... ok'; else echo 'test tests::x ... FAILED'; exit 1; fi").unwrap();
    std::fs::write(
        d.join("g.sh"),
        "echo '{\"Action\":\"run\",\"Package\":\"example.com/calc\",\"Test\":\"TestDiv\"}'; echo '{\"Action\":\"pass\",\"Package\":\"example.com/calc\",\"Test\":\"TestDiv\",\"Elapsed\":0}'",
    )
    .unwrap();
    std::fs::write(d.join("state"), "pass").unwrap();
    toe(&d, &["run", "rust_full"]);
    std::fs::write(d.join("state"), "fail").unwrap();
    let out = text(&toe(&d, &["run", "rust_quiet"]));
    assert!(
        out.contains("FAIL tests::x [")
            && out.contains("last passed at")
            && !out.contains("first time seen"),
        "quiet run knows the earlier pass: {out}"
    );
    toe(&d, &["run", "go_json"]);
    let out = text(&toe(&d, &["run", "go_text"]));
    assert!(
        out.contains("TestDiv [")
            && out.contains("last passed at")
            && !out.contains("first time seen"),
        "text run knows the json pass: {out}"
    );
}

/// Round 5: a test cut off by a cancel kept a garbled duplicate identity,
/// which `toe history` could not tell apart; it is shown as unfinished.
#[test]
fn a_cut_off_test_keeps_its_identity_and_is_shown_as_unfinished() {
    let d = workspace("cutoff");
    std::fs::write(
        d.join("toe.toml"),
        "[suite.go]\nrun = \"echo '=== RUN   TestA'; sleep 30\"\n",
    )
    .unwrap();
    let id = text(&toe(&d, &["run", "go", "--detach"])).trim().to_owned();
    toe(
        &d,
        &["wait", &id, "--until", "first-failure", "--max-wait", "2s"],
    );
    assert!(toe(&d, &["cancel", &id]).status.success());
    let s = text(&toe(
        &d,
        &["wait", &id, "--until", "done", "--max-wait", "10s"],
    ));
    assert!(s.contains("UNFINISHED TestA"), "{s}");
    assert!(!s.contains("FLAKY"), "{s}");
    let h = toe(&d, &["history", "TestA"]);
    assert!(h.status.success(), "one test, not ambiguous: {}", text(&h));
}
