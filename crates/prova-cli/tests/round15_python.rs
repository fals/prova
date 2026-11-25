//! Medium tests for round 15 findings on Python projects run through a
//! wrapper (`./dev`), each as a real prova process over a scripted output.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-r15py-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn home_of(dir: &Path) -> PathBuf {
    PathBuf::from(format!("{}.home", dir.display()))
}

fn prova(dir: &Path, args: &[&str]) -> Output {
    Command::new(PROVA)
        .args(args)
        .current_dir(dir)
        .env("PROVA_HOME", home_of(dir))
        .output()
        .expect("run prova")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(o)))
}

fn script(d: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let p = d.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Round 15: with `-v -rfE` the verbose FAILED line came first and the
/// message only in the short summary: the FAIL line carried none.
#[test]
fn a_verbose_failure_takes_its_message_from_the_short_summary() {
    let d = workspace("summary-msg");
    std::fs::write(
        d.join("out.txt"),
        "tests/test_x.py::test_a PASSED   [ 50%]\ntests/test_x.py::test_y FAILED   [100%]\n=========================== short test summary info ============================\nFAILED tests/test_x.py::test_y - AssertionError: 16 != 4\n========================= 1 failed, 1 passed in 0.03s ==========================\n",
    )
    .unwrap();
    let o = prova(
        &d,
        &["run", "--json", "--", "sh", "-c", "cat out.txt; exit 1"],
    );
    let v = json(&o);
    assert_eq!(
        v["failures"][0]["message"], "AssertionError: 16 != 4",
        "{v}"
    );
}

fn verify(d: &Path, suite: &str, filter: &str) -> String {
    let o = prova(d, &["adopt", "--verify", suite, "--filter", filter]);
    stdout(&o)
}

/// Round 15: a verify whose command could not run blamed the level.
#[test]
fn verify_names_why_the_command_could_not_run() {
    let d = workspace("verify-cause");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.gone]\nrun = \"./nonexistent-wrapper x\"\nselect = \"./nonexistent-wrapper {filter}\"\n\n[suite.nomod]\nrun = \"true\"\nselect = \"echo 'No module named pytest'; exit 1; echo {filter}\"\n",
    )
    .unwrap();
    let out = verify(&d, "gone", "t");
    assert!(
        out.contains("could not start") && out.contains("127") && !out.contains("level opaque"),
        "{out}"
    );
    let out = verify(&d, "nomod", "t");
    assert!(
        out.contains("No module named pytest") && !out.contains("level opaque"),
        "{out}"
    );
}

/// Round 15: pytest's "matched no test" got Maven's advice, and the split
/// filter went unnoticed.
#[test]
fn verify_advises_for_the_framework_and_spots_a_split_filter() {
    let d = workspace("verify-split");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.p]\nrun = \"true\"\nselect = \"echo 'pytest ERROR: file or directory not found: 2/-4]'; exit 4; echo {filter}\"\n",
    )
    .unwrap();
    let out = verify(&d, "p", "test_ratio[negative 2/-4]");
    assert!(
        out.contains("file or directory not found: 2/-4]")
            && out.contains("`-k`")
            && out.contains("split")
            && !out.contains("Maven"),
        "{out}"
    );
}

/// Round 15: adopt proposed `python3 -m pytest` on the host when the
/// project's own earlier run went through `./dev`.
#[test]
fn adopt_keeps_the_wrapper_of_earlier_runs() {
    let d = workspace("wrapper");
    std::fs::write(d.join("pyproject.toml"), "[tool.pytest.ini_options]\n").unwrap();
    std::fs::write(d.join("test_a.py"), "def test_a():\n    pass\n").unwrap();
    script(&d, "dev", "echo 'ran' \"$@\"");
    prova(&d, &["run", "--", "./dev", "make", "test"]);
    let o = prova(&d, &["adopt", "--json"]);
    let v = json(&o);
    let t = v["toml"].as_str().unwrap();
    assert!(t.contains("run = \"./dev python3 -m pytest"), "{t}");
    assert!(t.contains("select = \"./dev python3 -m pytest"), "{t}");
}

/// Round 15: pytest's exit 4 (a bad selection) was "FAILED exit 4" and
/// `prova run` exited 1, a test failure.
#[test]
fn pytest_exit_4_with_no_test_is_not_a_test_failure() {
    let d = workspace("exit4");
    let o = prova(
        &d,
        &[
            "run",
            "--json",
            "--",
            "sh",
            "-c",
            "echo 'ERROR: file or directory not found: 2/-4]'; exit 4",
        ],
    );
    let v = json(&o);
    assert_eq!(o.status.code(), Some(4), "{v}");
    assert_eq!(v["outcome"], "errored", "{v}");
    assert!(
        v["detail"].as_str().unwrap().contains("pytest ran no test"),
        "{v}"
    );
    // A real failure keeps exit 1.
    let o = prova(
        &d,
        &["run", "--json", "--", "sh", "-c", "echo 'boom'; exit 4"],
    );
    assert_eq!(o.status.code(), Some(1));
}

/// Round 15: tests that ran in docker were "unknown ... output came all at
/// once", and `history` had no size line.
#[cfg(target_os = "linux")]
#[test]
fn sizes_say_when_the_tests_ran_through_a_container_client() {
    let d = workspace("sizes-docker");
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    script(
        &bin,
        "docker",
        "i=0; while [ $i -lt 2000 ]; do i=$((i+1)); done\nprintf 'tests/test_a.py::test_one PASSED   [100%%]\\n'",
    );
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let o = Command::new(PROVA)
        .args(["run", "--", "docker", "run", "--rm", "img", "pytest", "-v"])
        .current_dir(&d)
        .env("PROVA_HOME", home_of(&d))
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stdout(&o));
    let out = stdout(&prova(&d, &["sizes"]));
    assert!(
        out.contains("ran through a container client") && !out.contains("came all at once"),
        "{out}"
    );
    let h = stdout(&prova(&d, &["history", "test_one"]));
    assert!(h.contains("size: unknown (ran through docker"), "{h}");
}

/// Round 15: an injected stream the test process never wrote (a wrapper
/// that does not forward the variable) was silent.
#[test]
fn an_injected_report_that_never_appears_is_warned_about() {
    let d = workspace("inject-lost");
    let v = json(&prova(
        &d,
        &[
            "run",
            "--json",
            "--inject",
            "pytest-reportlog",
            "--",
            "sh",
            "-c",
            "echo 'tests/t.py::test_a PASSED  [100%]'",
        ],
    ));
    let w = v["warnings"].to_string();
    assert!(
        w.contains("pytest-reportlog report was never written")
            && w.contains("did not reach the test process"),
        "{v}"
    );
}

/// Round 17 (python-make): a failed run whose pytest-reportlog stream never
/// came (the report was written inside a container) had no warning, fell
/// to level opaque and said "2 failed" and "prova saw 0 individually". The
/// warning is given for a failed run too, and the counts agree.
#[test]
fn a_failed_run_whose_stream_never_came_warns_and_counts_consistently() {
    let d = workspace("inject-lost-failed");
    let v = json(&prova(
        &d,
        &[
            "run",
            "--json",
            "--inject",
            "pytest-reportlog",
            "--",
            "sh",
            "-c",
            "printf '=== short test summary info ===\\nFAILED tests/t.py::test_a - assert 1\\nFAILED tests/t.py::test_b - assert 2\\n=== 2 failed in 0.10s ===\\n'; exit 1",
        ],
    ));
    assert!(
        v["warnings"]
            .to_string()
            .contains("report was never written"),
        "{v}"
    );
    assert_eq!(v["level"], "recognized", "{v}");
    let text = v.to_string();
    assert!(
        !text.contains("individually") && !text.contains("but prova recorded 0"),
        "{v}"
    );
}

/// Round 15: at a stall, `pytest -v` had printed `test_slow PASSED` and the
/// name of the test that hangs, without its outcome. The pass counts at
/// once and `running now` names the hanging test.
#[test]
fn a_pytest_stall_names_the_running_test_and_counts_the_pass() {
    let d = workspace("stall-partial");
    let o = prova(
        &d,
        &[
            "run",
            "--detach",
            "--json",
            "--stall-after",
            "1s",
            "--",
            "sh",
            "-c",
            "echo 'tests/t.py::test_slow PASSED   [ 50%]'; printf 'tests/t.py::test_hangs '; exec sleep 60",
        ],
    );
    let id = json(&o)["run"].as_str().unwrap().to_owned();
    let w = prova(&d, &["wait", &id, "--max-wait", "20s", "--json"]);
    let s = json(&w);
    prova(&d, &["cancel", &id]);
    prova(&d, &["wait", &id, "--until", "done", "--max-wait", "20s"]);
    assert_eq!(s["reason"], "stall", "{s}");
    assert_eq!(s["counts"]["passed"], 1, "{s}");
    assert_eq!(s["running"][0], "tests.t::test_hangs", "{s}");
}

/// Round 18 review: the printed command of an argv run is shell-quoted so
/// it can be pasted back, in the run header as in the JSON; parsing it (the
/// filter advice) uses the words.
#[test]
fn the_header_of_an_argv_run_shows_its_quoted_word() {
    let d = workspace("quotedheader");
    let o = prova(&d, &["run", "--", "echo", "cd sub && pytest", "it's"]);
    let out = stdout(&o);
    assert!(
        out.contains(r"`echo 'cd sub && pytest' 'it'\''s'`"),
        "{out}"
    );
    let o = prova(&d, &["run", "--json", "--", "echo", "cd sub && pytest"]);
    assert_eq!(json(&o)["command"], "echo 'cd sub && pytest'");
}
