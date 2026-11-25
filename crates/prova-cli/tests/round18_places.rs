//! Medium tests for ADR 0034 sections 2 and 3 (usability round 18): a test's
//! place comes from the repository, and history compares like with like.
//! Real runs are needed: names come from a command's output and are compared
//! with the store's history across runs started in different directories.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-r18pl-{name}-{}", std::process::id()));
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

/// Runs prova in `dir`; the home is the repository's (`repo`), so runs
/// started in different directories share one history.
fn prova_in(repo: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(PROVA)
        .args(args)
        .current_dir(dir)
        .env("PROVA_HOME", format!("{}.home", repo.display()))
        .output()
        .expect("run prova")
}

fn prova(dir: &Path, args: &[&str]) -> Output {
    prova_in(dir, dir, args)
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Some real work first: prova does not believe results printed by a
/// command that used no CPU (round 10).
const BUSY: &str = "b=0; while [ $b -lt 60000 ]; do b=$((b+1)); done;";

/// Round 18 (ts-vitest): `cd packages/b && prova run -- npm test` named
/// tests from the directory prova started in, so two packages' (and the
/// root's) `src/a.test.ts` were one test in one history. Why medium: it
/// needs two runs started in different directories of one repository.
#[test]
fn r18_tests_are_named_from_the_repository_root_whatever_directory_prova_started_in() {
    let d = workspace("repo");
    std::fs::create_dir_all(d.join("pkg")).unwrap();
    let run = |root: &Path| {
        format!(
            "{BUSY} printf '%s\\n' '' ' RUN  v3.2.4 {}' '' ' × src/a.test.ts > works 1ms' '' ' Test Files  1 failed (1)' '      Tests  1 failed (1)'; exit 1",
            root.display()
        )
    };
    let o = prova_in(
        &d,
        &d.join("pkg"),
        &["run", "--", "sh", "-c", &run(&d.join("pkg"))],
    );
    let out = text(&o);
    assert!(out.contains("FAIL pkg/src/a.test.ts::works"), "{out}");
    let o = prova(&d, &["run", "--", "sh", "-c", &run(&d)]);
    let out = text(&o);
    assert!(
        out.contains("FAIL src/a.test.ts::works") && !out.contains("FLAKY"),
        "{out}"
    );
    let all = text(&prova(&d, &["history", ""]));
    assert!(
        all.contains("pkg/src/a.test.ts::works") && all.contains("  src/a.test.ts::works"),
        "{all}"
    );
}

/// The same for pytest, whose ids are relative to the directory it runs in.
#[test]
fn r18_pytest_ids_run_from_a_subdirectory_carry_its_path() {
    let d = workspace("pyrepo");
    std::fs::create_dir_all(d.join("pkg/tests")).unwrap();
    std::fs::write(d.join("pkg/tests/test_x.py"), "").unwrap();
    let cmd = format!(
        "{BUSY} printf '%s\\n' 'tests/test_x.py::test_a FAILED [100%]' '=== 1 failed in 0.01s ==='; exit 1"
    );
    let o = prova_in(&d, &d.join("pkg"), &["run", "--", "sh", "-c", &cmd]);
    let out = text(&o);
    assert!(out.contains("FAIL pkg.tests.test_x::test_a"), "{out}");
}

/// What quiet `cargo test` prints for one failing unit test: no crate, only
/// the panic message's path.
fn quiet_cargo_failure(crate_dir: &str) -> String {
    format!(
        "{BUSY} printf '%s\\n' '.F' 'tests::flaky_clock --- FAILED' '' 'failures:' '' '---- tests::flaky_clock stdout ----' \"thread 'tests::flaky_clock' panicked at {crate_dir}/src/lib.rs:29:9:\" 'boom' '' 'failures:' '    tests::flaky_clock' '' 'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s'; exit 101"
    )
}

/// Round 18 (rust-make): quiet `cargo test` names `tests::flaky_clock` with
/// no crate, and geo's real failure took calc's history (FLAKY). The panic
/// message's path gives the crate. Why medium: the crate is found on disk.
#[test]
fn r18_a_quiet_cargo_failure_is_told_by_its_panic_path_and_never_borrows() {
    let d = workspace("quietcargo");
    for krate in ["calc", "geo"] {
        std::fs::create_dir_all(d.join(krate).join("src")).unwrap();
        std::fs::write(
            d.join(krate).join("Cargo.toml"),
            format!("[package]\nname = \"{krate}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
        std::fs::write(d.join(krate).join("src/lib.rs"), "").unwrap();
    }
    let o = prova(&d, &["run", "--", "sh", "-c", &quiet_cargo_failure("calc")]);
    let out = text(&o);
    assert!(out.contains("FAIL tests::flaky_clock"), "{out}");
    let o = prova(&d, &["run", "--", "sh", "-c", &quiet_cargo_failure("geo")]);
    let out = text(&o);
    assert!(
        out.contains("first time seen") && !out.contains("FLAKY") && !out.contains("before"),
        "geo's failure borrowed calc's history: {out}"
    );
    let all = text(&prova(&d, &["history", "tests::flaky_clock"]));
    assert!(
        all.contains("(calc)") && all.contains("(geo)"),
        "each crate's test is its own: {all}"
    );
}
