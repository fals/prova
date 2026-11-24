//! Medium tests for round 16 findings on projects run through a wrapper
//! (`./dev`), each as a real prova process.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-r16-{name}-{}", std::process::id()));
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

/// Round 16: `prova run -- ./dev "dotnet test"` (the whole command as one
/// argument) left adopt proposing the bare `dotnet test`.
#[test]
fn adopt_keeps_a_wrapper_that_takes_the_whole_command_quoted() {
    let d = workspace("quoted-wrapper");
    std::fs::write(d.join("App.sln"), "").unwrap();
    script(&d, "dev", "echo ran \"$@\"");
    prova(&d, &["run", "--", "./dev", "dotnet test"]);
    let v = json(&prova(&d, &["adopt", "--json"]));
    let t = v["toml"].as_str().unwrap();
    assert!(
        t.contains("run = \"./dev \\\"dotnet test --logger trx"),
        "{t}"
    );
    assert!(t.contains("verbosity=normal"), "{t}");
}

/// Round 16: the newest run alone decided the wrapper, and a project whose
/// recent runs mostly went through `./dev` lost it after one bare run.
#[test]
fn adopt_takes_the_wrapper_most_runs_used() {
    let d = workspace("frequent-wrapper");
    std::fs::write(d.join("pyproject.toml"), "[tool.pytest.ini_options]\n").unwrap();
    script(&d, "dev", "echo ran \"$@\"");
    for _ in 0..3 {
        prova(&d, &["run", "--", "./dev", "make", "test"]);
    }
    prova(&d, &["run", "--", "echo", "pytest"]);
    let v = json(&prova(&d, &["adopt", "--json"]));
    let t = v["toml"].as_str().unwrap();
    assert!(t.contains("run = \"./dev python3 -m pytest"), "{t}");
}

/// Round 16: tests run in docker (out of sight) under a suite that declares
/// `size = "small"` still read "unknown ... declare their size".
#[cfg(target_os = "linux")]
#[test]
fn a_declared_size_is_reported_for_tests_out_of_sight_in_a_container() {
    let d = workspace("declared-size");
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    script(
        &bin,
        "docker",
        "i=0; while [ $i -lt 2000 ]; do i=$((i+1)); done\nprintf 'tests/test_a.py::test_one PASSED   [100%%]\\n'",
    );
    std::fs::write(
        d.join("prova.toml"),
        "[suite.default]\nrun = \"docker run --rm img pytest -v\"\nsize = \"small\"\n",
    )
    .unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let o = Command::new(PROVA)
        .args(["run", "default"])
        .current_dir(&d)
        .env("PROVA_HOME", home_of(&d))
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stdout(&o));
    let out = stdout(&prova(&d, &["sizes"]));
    assert!(
        out.contains("small 1") && out.contains("unknown 0") && !out.contains("declare their size"),
        "{out}"
    );
    let h = stdout(&prova(&d, &["history", "test_one"]));
    assert!(h.contains("small (declared; out of sight in docker"), "{h}");
}

/// Round 16: `inject = ["pytest-junit"]` through a wrapper that did not
/// forward the variable said "injected" and nothing else; a container needs
/// the variable forwarded and the path mounted.
#[cfg(target_os = "linux")]
#[test]
fn a_result_file_report_that_never_appears_is_warned_about() {
    let d = workspace("junit-lost");
    let v = json(&prova(
        &d,
        &[
            "run",
            "--json",
            "--inject",
            "pytest-junit",
            "--",
            "sh",
            "-c",
            "echo 'tests/t.py::test_a PASSED  [100%]'",
        ],
    ));
    let w = v["warnings"].to_string();
    assert!(
        w.contains("pytest-junit report was never written")
            && !w.contains("forwarded into the container"),
        "{v}"
    );
    // Through a container client: what it needs to work.
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    script(&bin, "docker", "echo 'tests/t.py::test_a PASSED  [100%]'");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let o = Command::new(PROVA)
        .args([
            "run",
            "--json",
            "--inject",
            "pytest-junit",
            "--",
            "docker",
            "run",
            "--rm",
            "img",
            "pytest",
        ])
        .current_dir(&d)
        .env("PROVA_HOME", home_of(&d))
        .env("PATH", path)
        .output()
        .unwrap();
    let w = json(&o)["warnings"].to_string();
    assert!(
        w.contains("never written")
            && w.contains("forwarded into the container")
            && w.contains("mounted"),
        "{w}"
    );
}

/// Round 16: a filter with spaces split by a wrapper ended "pytest ran no
/// test" without the hint `adopt --verify` gives.
#[test]
fn a_run_says_when_a_wrapper_split_the_filter_into_words() {
    let d = workspace("split-filter");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.default]\nrun = \"echo all\"\nselect = \"echo 'ERROR: file or directory not found: 2/-4]'; exit 4 # {filter}\"\n",
    )
    .unwrap();
    let o = prova(
        &d,
        &[
            "run",
            "default",
            "--select",
            "test_add_cases[negative 2/-4]",
        ],
    );
    let out = stdout(&o);
    assert!(
        out.contains("split into words by a wrapper") && out.contains("`-k`"),
        "{out}"
    );
}
