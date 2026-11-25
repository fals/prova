//! Medium tests for round 17 (counts, history, wording): real runs are
//! needed because history and labels come from the store.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-r17-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(".gitignore"), "state\n").unwrap();
    let vcs = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&d)
            .output()
            .unwrap()
    };
    vcs(&["init", "-q"]);
    vcs(&["add", "-A"]);
    vcs(&[
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

/// A little real work first: prova does not believe results printed by a
/// command that used no CPU (round 10).
const WORK: &str = "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done; ";

fn suite(d: &Path, body: &str) {
    std::fs::write(d.join("prova.toml"), body).unwrap();
}

/// Why medium: history reads runs the store recorded.
#[test]
fn history_suite_option_limits_the_runs() {
    let d = workspace("suite");
    let ok = format!("{WORK}echo 'test calc::x ... ok'");
    let bad = format!("{WORK}echo 'test calc::x ... FAILED'; exit 1");
    suite(
        &d,
        &format!("[suite.make]\nrun = {bad:?}\n\n[suite.default]\nrun = {ok:?}\n"),
    );
    prova(&d, &["run", "make"]);
    prova(&d, &["run", "default"]);
    let all = text(&prova(&d, &["history", "calc::x"]));
    assert!(all.contains("2 runs with a result, 1 failed"), "{all}");
    let one = text(&prova(&d, &["history", "calc::x", "--suite", "default"]));
    assert!(one.contains("1 runs with a result, 0 failed"), "{one}");
}

#[test]
fn history_with_an_empty_name_lists_every_test() {
    let d = workspace("empty");
    let cmd = format!("{WORK}echo 'test calc::a ... ok'; echo 'test calc::b ... ok'");
    suite(&d, &format!("[suite.default]\nrun = {cmd:?}\n"));
    prova(&d, &["run", "default"]);
    let o = prova(&d, &["history", ""]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(out.contains("calc::a") && out.contains("calc::b"), "{out}");
    assert!(!out.contains("be more specific"), "{out}");
}

/// A pytest verbose line has no message and no time; the summary line names
/// the cause. History shows the cause, and says the time is unknown.
#[test]
fn history_keeps_the_failure_message_and_does_not_invent_a_duration() {
    let d = workspace("msg");
    let cmd = format!(
        "{WORK}echo 'tests/t.py::test_div FAILED  [ 50%]'; echo 'tests/t.py::test_ok PASSED  [100%]'; echo 'FAILED tests/t.py::test_div - assert 1 == 2'; echo '=== 1 failed, 1 passed in 0.1s ==='; exit 1"
    );
    suite(&d, &format!("[suite.default]\nrun = {cmd:?}\n"));
    prova(&d, &["run", "default"]);
    let out = text(&prova(&d, &["history", "test_div"]));
    assert!(out.contains("assert 1 == 2"), "{out}");
    let ok = text(&prova(&d, &["history", "test_ok"]));
    assert!(!ok.contains("p50 0ms"), "{ok}");
}
