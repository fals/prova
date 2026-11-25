//! Medium tests for ADR 0035 sections 1 and 2 (usability round 18): a test's
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

/// Review of PR 115 (B): a project in a subdirectory with its own
/// `prova.toml` is its own root: its tests keep their `src/...` names (and
/// the history recorded under them), though the git work tree is above.
/// Why medium: needs a git work tree and a run started in the subproject.
#[test]
fn r18_a_subproject_with_its_own_prova_toml_names_tests_from_it() {
    let d = workspace("subproject");
    let api = d.join("services/api");
    std::fs::create_dir_all(&api).unwrap();
    std::fs::write(api.join("prova.toml"), "").unwrap();
    let cmd = format!(
        "{BUSY} printf '%s\\n' '' ' RUN  v3.2.4 {}' '' ' × src/x.test.ts > works 1ms' '' ' Test Files  1 failed (1)' '      Tests  1 failed (1)'; exit 1",
        api.display()
    );
    let out = text(&prova_in(&d, &api, &["run", "--", "sh", "-c", &cmd]));
    assert!(
        out.contains("FAIL src/x.test.ts::works") && !out.contains("services/api/src"),
        "{out}"
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

/// Review of PR 115 (A): a panic path is a place only when it lies inside
/// the repository, outside `.cargo/`, and the file holds the test's own
/// function. A panic in a dependency, in a helper crate, or in the library
/// code an integration test calls leaves the place unknown: the failure
/// borrows no crate's history and is named with no crate. Why medium: the
/// files are found on disk.
#[test]
fn r18_a_panic_path_that_is_not_the_tests_own_file_leaves_the_place_unknown() {
    type Case<'a> = (&'a str, Vec<(&'a str, &'a str)>, String);
    let cases: [Case; 4] = [
        (
            "dependency under .cargo",
            vec![
                (
                    ".cargo/registry/src/x/foo-1.0/Cargo.toml",
                    "[package]\nname = \"foo\"\nversion = \"1.0.0\"\n",
                ),
                (
                    ".cargo/registry/src/x/foo-1.0/src/lib.rs",
                    "fn flaky_clock() {}\n",
                ),
            ],
            ".cargo/registry/src/x/foo-1.0/src/lib.rs".to_owned(),
        ),
        (
            "helper crate",
            vec![
                (
                    "testutil/Cargo.toml",
                    "[package]\nname = \"testutil\"\nversion = \"0.1.0\"\n",
                ),
                ("testutil/src/lib.rs", "pub fn helper() {}\n"),
            ],
            "testutil/src/lib.rs".to_owned(),
        ),
        (
            "integration test panicking in library code",
            vec![
                (
                    "calc/Cargo.toml",
                    "[package]\nname = \"calc\"\nversion = \"0.1.0\"\n",
                ),
                ("calc/src/lib.rs", "pub fn helper() {}\n"),
                ("calc/tests/it.rs", "#[test]\nfn flaky_clock() {}\n"),
            ],
            "calc/src/lib.rs".to_owned(),
        ),
        (
            "outside the repository",
            vec![],
            String::new(), // filled in below: a sibling directory
        ),
    ];
    for (i, (what, files, path)) in cases.into_iter().enumerate() {
        let d = workspace(&format!("panicpath{i}"));
        for krate in ["geo", "alpha"] {
            std::fs::create_dir_all(d.join(krate).join("src")).unwrap();
            std::fs::write(
                d.join(krate).join("Cargo.toml"),
                format!("[package]\nname = \"{krate}\"\nversion = \"0.1.0\"\n"),
            )
            .unwrap();
            std::fs::write(d.join(krate).join("src/lib.rs"), "fn flaky_clock() {}\n").unwrap();
        }
        let mut path = path;
        if files.is_empty() {
            let outside = PathBuf::from(format!("{}.dep", d.display()));
            std::fs::create_dir_all(outside.join("src")).unwrap();
            std::fs::write(
                outside.join("Cargo.toml"),
                "[package]\nname = \"dep\"\nversion = \"0.1.0\"\n",
            )
            .unwrap();
            std::fs::write(outside.join("src/lib.rs"), "fn flaky_clock() {}\n").unwrap();
            path = format!("{}/src/lib.rs", outside.display());
        }
        for (f, c) in &files {
            let p = d.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        }
        let at = |p: &str| {
            format!(
                "{BUSY} printf '%s\\n' '.F' 'tests::flaky_clock --- FAILED' '' 'failures:' '' '---- tests::flaky_clock stdout ----' \"thread 'tests::flaky_clock' panicked at {p}:29:9:\" 'boom' '' 'failures:' '    tests::flaky_clock' '' 'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s'; exit 101"
            )
        };
        // geo's own failure: the history a misplaced one would borrow.
        for h in ["geo", "alpha"] {
            prova(
                &d,
                &["run", "--", "sh", "-c", &at(&format!("{h}/src/lib.rs"))],
            );
        }
        let out = text(&prova(&d, &["run", "--", "sh", "-c", &at(&path)]));
        let line = out
            .lines()
            .find(|l| l.trim_start().starts_with("FAIL "))
            .unwrap_or_else(|| panic!("{what}: no FAIL line: {out}"));
        assert!(
            line.contains("[first time seen")
                && !line.contains("FLAKY")
                && !line.contains("flaky_clock ("),
            "{what}: the place stays unknown: {out}"
        );
    }
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
        std::fs::write(
            d.join(krate).join("src/lib.rs"),
            "#[test]\nfn flaky_clock() {}\n",
        )
        .unwrap();
    }
    let o = prova(&d, &["run", "--", "sh", "-c", &quiet_cargo_failure("calc")]);
    let out = text(&o);
    assert!(out.contains("FAIL tests::flaky_clock"), "{out}");
    let o = prova(&d, &["run", "--", "sh", "-c", &quiet_cargo_failure("geo")]);
    let out = text(&o);
    assert!(
        out.lines()
            .find(|l| l.trim_start().starts_with("FAIL "))
            .is_some_and(|l| l.contains("[first time seen") && !l.contains("FLAKY")),
        "geo's failure borrowed calc's history: {out}"
    );
    let all = text(&prova(&d, &["history", "tests::flaky_clock"]));
    assert!(
        all.contains("(calc)") && all.contains("(geo)"),
        "each crate's test is its own: {all}"
    );
}

/// Round 18 (go-make): `go test -json -count=4` with FAIL, FAIL, FAIL, PASS
/// was "broken, not flaky" in history; the `-v` text said FLAKY. Why
/// medium: the label comes from stored attempts.
#[test]
fn r18_go_json_count_with_a_late_pass_is_flaky_in_history() {
    let d = workspace("gojsoncount");
    let mut events = String::new();
    for a in ["fail", "fail", "fail", "pass"] {
        events.push_str(&format!(
            "{{\"Action\":\"run\",\"Package\":\"ex/a\",\"Test\":\"TestFlaky\"}}\n{{\"Action\":\"{a}\",\"Package\":\"ex/a\",\"Test\":\"TestFlaky\",\"Elapsed\":0.01}}\n"
        ));
    }
    events.push_str("{\"Action\":\"fail\",\"Package\":\"ex/a\",\"Elapsed\":0.1}\n");
    std::fs::write(d.join("ev.json"), events).unwrap();
    let cmd = format!("{BUSY} cat ev.json; exit 1");
    prova(&d, &["run", "--", "sh", "-c", &cmd]);
    let h = text(&prova(&d, &["history", "TestFlaky"]));
    assert!(
        h.contains("both passed and failed on 1 code version"),
        "{h}"
    );
    let f = text(&prova(&d, &["flaky"]));
    assert!(
        f.contains("TestFlaky"),
        "prova flaky agrees with history: {f}"
    );
}

/// Round 18 (probe M3): a run whose only failure was a quarantined one
/// counted as a pass on this exact code and made another place's real
/// failure "likely FLAKY: suite passed on this exact code before". A
/// quarantine never turns a failed run green (ADR 0034), so such a run is
/// no pass on this code. Why medium: needs a quarantine, a stored run and
/// the same code across runs.
#[test]
fn r18_a_failed_run_with_only_quarantined_failures_is_not_a_pass_on_this_code() {
    use std::os::unix::fs::PermissionsExt;
    let d = workspace("setaside");
    let out = PathBuf::from(format!("{}.out", d.display()));
    std::fs::create_dir_all(&out).unwrap();
    let nextest = |calc: &str, geo: &str| {
        format!(
            "    Starting 2 tests across 2 binaries\n        {calc} [   0.004s] calc tests::flaky\n        {geo} [   0.004s] geo tests::real_bug\n     Summary [   0.020s] 2 tests run\n"
        )
    };
    std::fs::create_dir_all(d.join("rc")).unwrap();
    let script = d.join("rc/cargo");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n{BUSY}\ncat {o}/out.txt\nexit 100\n",
            o = out.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        d.join("prova.toml"),
        "[suite.s]\nrun = \"./rc/cargo nextest run\"\n",
    )
    .unwrap();
    std::fs::write(out.join("out.txt"), nextest("FAIL", "PASS")).unwrap();
    prova(&d, &["run", "s"]);
    let o = prova(
        &d,
        &[
            "quarantine",
            "add",
            "tests::flaky",
            "--in",
            "calc",
            "--reason",
            "r",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    // calc fails again, quarantined: the run still fails.
    let o = prova(&d, &["run", "s"]);
    assert_ne!(o.status.code(), Some(0), "{}", text(&o));
    // geo's own failure of another test.
    std::fs::write(out.join("out.txt"), nextest("PASS", "FAIL")).unwrap();
    let o = prova(&d, &["run", "s"]);
    let o = text(&o);
    assert!(o.contains("FAIL tests::real_bug"), "{o}");
    assert!(!o.contains("likely FLAKY"), "{o}");
}

/// Round 18 (go-make MAJOR 1): `--repeat` over `go test -json -count=4`
/// whose test failed three times and passed once in every run said "failed
/// every time ... broken, not flaky" for it, though the same stream says
/// FLAKY in history. The pass of a later iteration counts in the tally. Why
/// medium: needs `--repeat` over a real stored run.
#[test]
fn r18_repeat_over_go_json_count_tallies_the_late_pass() {
    let d = workspace("gojsonrepeat");
    let mut events = String::new();
    for a in ["fail", "fail", "fail", "pass"] {
        events.push_str(&format!(
            "{{\"Action\":\"run\",\"Package\":\"ex/a\",\"Test\":\"TestFlaky\"}}\n{{\"Action\":\"{a}\",\"Package\":\"ex/a\",\"Test\":\"TestFlaky\",\"Elapsed\":0.01}}\n"
        ));
    }
    events.push_str("{\"Action\":\"fail\",\"Package\":\"ex/a\",\"Elapsed\":0.1}\n");
    std::fs::write(d.join("ev.json"), events).unwrap();
    let cmd = format!("{BUSY} cat ev.json; exit 1");
    let o = text(&prova(
        &d,
        &["run", "--repeat", "2", "--", "sh", "-c", &cmd],
    ));
    assert!(
        o.contains("FLAKY ex::TestFlaky") || o.contains("FLAKY ex/a::TestFlaky"),
        "{o}"
    );
    assert!(!o.contains("failed every time"), "{o}");
    assert!(!o.contains("broken, not flaky"), "{o}");
}

/// Round 18 (rust NIT): `slowest` cut its list without saying so. Why
/// medium: needs stored durations.
#[test]
fn r18_slowest_says_how_many_it_cut() {
    let d = workspace("slowestmore");
    let cmd = format!(
        "{BUSY} printf '%s\\n' '    Starting 3 tests across 1 binary' '        PASS [   0.300s] calc tests::t1' '        PASS [   0.200s] calc tests::t2' '        PASS [   0.100s] calc tests::t3' '     Summary [   0.600s] 3 tests run: 3 passed, 0 skipped'"
    );
    prova(&d, &["run", "--", "sh", "-c", &cmd]);
    let all = text(&prova(&d, &["slowest", "-n", "10"]));
    assert!(!all.contains("more"), "{all}");
    let one = text(&prova(&d, &["slowest", "-n", "1"]));
    assert!(one.contains("... and 2 more"), "{one}");
}

/// Round 18 (rust M2, go MINOR 3): `history <name>` hid the other tests the
/// name matches, and a Go parent was "ambiguous" with its own subtests.
/// Why medium: it reads stored tests.
#[test]
fn r18_history_says_what_else_a_name_matches_and_the_exact_leaf_wins() {
    let d = workspace("histmatch");
    let rust = format!(
        "{BUSY} printf '%s\\n' 'test tests::flaky_clock ... ok' 'test flaky_clock ... ok' 'test result: ok. 2 passed; 0 failed; 0 ignored'"
    );
    prova(&d, &["run", "--", "sh", "-c", &rust]);
    let h = text(&prova(&d, &["history", "flaky_clock"]));
    assert!(h.contains("runs with a result"), "{h}");
    assert!(
        h.contains("also matches 1 other test(s): tests::flaky_clock"),
        "{h}"
    );
    let go = format!(
        "{BUSY} printf '%s\\n' '=== RUN   TestMod' '=== RUN   TestMod/a' '    --- PASS: TestMod/a (0.00s)' '--- PASS: TestMod (0.00s)' 'PASS' 'ok  \tex/a\t0.01s'"
    );
    prova(&d, &["run", "--", "sh", "-c", &go]);
    let h = text(&prova(&d, &["history", "TestMod"]));
    assert!(h.contains("runs with a result"), "{h}");
    assert!(!h.contains("be more specific"), "{h}");
}

/// Round 18 (probe N3, rust MINOR/NIT): `quarantine remove TestFlaky --in b`
/// failed for Go; Rust `--in calc` was refused for an integration test
/// (`calc::it`); the places listed `tests`, a module; a bare name that two
/// crates' integration tests share was "also known qualified". Why medium:
/// stored tests and quarantines.
#[test]
fn r18_quarantine_in_takes_go_packages_and_rust_crates() {
    let d = workspace("qin");
    let go = format!(
        "{BUSY} printf '%s\\n' '=== RUN   TestFlaky' '--- PASS: TestFlaky (0.00s)' 'PASS' 'ok  \tex/a\t0.01s' '=== RUN   TestFlaky' '--- FAIL: TestFlaky (0.00s)' 'FAIL' 'FAIL\tex/b\t0.01s'; exit 1"
    );
    prova(&d, &["run", "--", "sh", "-c", &go]);
    for p in ["a", "b"] {
        let o = prova(
            &d,
            &["quarantine", "add", "TestFlaky", "--in", p, "--reason", "r"],
        );
        assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    }
    let o = prova(&d, &["quarantine", "remove", "TestFlaky", "--in", "b"]);
    assert_eq!(o.status.code(), Some(0), "go remove --in: {}", text(&o));
    let rust = format!(
        "{BUSY} printf '%s\\n' '    Starting 4 tests across 2 binaries' '        PASS [   0.004s] calc::it flaky_clock' '        PASS [   0.004s] geo::it flaky_clock' '        PASS [   0.004s] calc tests::flaky_clock' '        PASS [   0.004s] geo tests::flaky_clock' '     Summary [   0.020s] 4 tests run'"
    );
    prova(&d, &["run", "--", "sh", "-c", &rust]);
    let o = text(&prova(
        &d,
        &["quarantine", "add", "flaky_clock", "--reason", "r"],
    ));
    assert!(!o.contains("also known qualified"), "{o}");
    assert!(o.contains("calc::it") && o.contains("--in"), "{o}");
    let o = prova(
        &d,
        &[
            "quarantine",
            "add",
            "flaky_clock",
            "--in",
            "calc",
            "--reason",
            "r",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "rust --in crate: {}", text(&o));
    let o = text(&prova(
        &d,
        &[
            "quarantine",
            "add",
            "tests::flaky_clock",
            "--in",
            "nowhere",
            "--reason",
            "r",
        ],
    ));
    assert!(o.contains("it is in: calc, geo"), "{o}");
}

/// Round 18 (rust MINOR): the live SLOW line dropped the crate that the
/// UNFINISHED line showed. Why medium: needs a run still going.
#[test]
fn r18_the_slow_line_names_the_crate() {
    let d = workspace("slowcrate");
    let cmd = format!(
        "{BUSY} printf '%s\\n' '    Starting 1 tests across 1 binaries' '        SLOW [> 60.000s] calc tests::hangs'; while [ ! -f {d}/stop ]; do sleep 0.2; done",
        d = d.display()
    );
    let o = text(&prova(&d, &["run", "--detach", "--", "sh", "-c", &cmd]));
    let i = o.find("run ").unwrap() + 4;
    let id = o[i..i + 8].to_owned();
    let w = text(&prova(
        &d,
        &["wait", &id, "--until", "first-failure", "--max-wait", "10s"],
    ));
    std::fs::write(d.join("stop"), "").unwrap();
    prova(&d, &["cancel", &id]);
    assert!(w.contains("SLOW tests::hangs (calc)"), "{w}");
}

/// Round 18 (rust MINOR/NIT): `sizes` said "unknown 2" without naming
/// them. Why medium: stored runs.
#[test]
fn r18_sizes_names_the_unknown() {
    let d = workspace("sizesslow");
    let mut lines = String::new();
    for i in 0..12 {
        lines.push_str(&format!("'test m::t{i} ... ok' "));
    }
    let cmd =
        format!("{BUSY} printf '%s\\n' {lines} 'test result: ok. 12 passed; 0 failed; 0 ignored'");
    prova(&d, &["run", "--", "sh", "-c", &cmd]);
    let s = text(&prova(&d, &["sizes"]));
    assert!(s.contains("m::t0") && s.contains("unknown"), "{s}");
}

/// Round 18 (go NIT 2): a Go test that took 0.00s read "duration unknown";
/// 0 ms is a duration. Why medium: stored runs.
#[test]
fn r18_zero_milliseconds_is_a_duration() {
    let d = workspace("zeroms");
    let go = format!(
        "{BUSY} printf '%s\\n' '=== RUN   TestQuick' '--- PASS: TestQuick (0.00s)' 'PASS' 'ok  \tex/a\t0.01s'"
    );
    prova(&d, &["run", "--", "sh", "-c", &go]);
    prova(&d, &["run", "--", "sh", "-c", &go]);
    let h = text(&prova(&d, &["history", "TestQuick"]));
    assert!(!h.contains("duration unknown"), "{h}");
    assert!(h.contains("p50 <1ms"), "{h}");
}

/// Round 18 (go NIT 1): a parent counted fewer runs than its subtest: the
/// run that selected only the subtest was skipped even where the parent
/// failed. Only a pass there is no evidence about the parent.
#[test]
fn r18_a_subtest_selection_run_still_counts_the_parents_failure() {
    let d = workspace("subsel");
    std::fs::write(
        d.join("t.sh"),
        format!(
            "{BUSY}\nprintf '%s\\n' '=== RUN   TestP' '=== RUN   TestP/a' '    --- FAIL: TestP/a (0.00s)' '--- FAIL: TestP (0.00s)' 'FAIL' 'FAIL\tex/a\t0.01s'\nexit 1\n"
        ),
    )
    .unwrap();
    std::fs::write(
        d.join("prova.toml"),
        "[suite.s]\nrun = \"sh t.sh\"\nselect = \"sh t.sh {filter}\"\n",
    )
    .unwrap();
    prova(&d, &["run", "s"]);
    prova(&d, &["run", "s", "--select", "TestP/a"]);
    let h = text(&prova(&d, &["history", "TestP"]));
    assert!(h.contains("2 runs with a result"), "{h}");
}

/// Round 18 (probe M4): after one `-count=2` fail-then-pass and failures
/// that run on, `prova flaky` and `prova history` must agree.
#[test]
fn r18_flaky_and_history_agree_when_failures_run_on() {
    let d = workspace("agree");
    let go =
        |lines: &str| format!("{BUSY} printf '%s\\n' {lines} 'FAIL' 'FAIL\tex/a\t0.01s'; exit 1");
    let mixed = go(
        "'=== RUN   TestM' '--- FAIL: TestM (0.00s)' '=== RUN   TestM' '--- PASS: TestM (0.00s)'",
    );
    let fail = go(
        "'=== RUN   TestM' '--- FAIL: TestM (0.00s)' '=== RUN   TestM' '--- FAIL: TestM (0.00s)'",
    );
    prova(&d, &["run", "--", "sh", "-c", &mixed]);
    for n in 0..6 {
        prova(&d, &["run", "--", "sh", "-c", &fail]);
        let h = text(&prova(&d, &["history", "TestM"]));
        let f = text(&prova(&d, &["flaky"]));
        assert_eq!(
            h.contains("=> FLAKY"),
            f.contains("TestM"),
            "after {n} more failures: history says\n{h}\nflaky says\n{f}"
        );
        if n == 5 {
            assert!(h.contains("failing now"), "{h}");
        }
    }
}

/// Round 18 (rust-make 3179c700): a quiet run cut short named a test of
/// unknown place; both crates' quarantines then read "failed". Neither ran.
#[test]
fn r18_a_failure_of_unknown_place_is_no_quarantined_place_failing() {
    let d = workspace("stalledplaces");
    let nextest = format!(
        "{BUSY} printf '%s\\n' '    Starting 2 tests across 2 binaries' '        PASS [   0.004s] calc tests::flaky_clock' '        PASS [   0.004s] geo tests::flaky_clock' '     Summary [   0.020s] 2 tests run'"
    );
    let quiet = format!("{BUSY} printf '%s\\n' '.F' 'tests::flaky_clock --- FAILED'; exit 101");
    prova(&d, &["run", "--", "sh", "-c", &nextest]);
    for p in ["calc", "geo"] {
        let o = prova(
            &d,
            &[
                "quarantine",
                "add",
                "tests::flaky_clock",
                "--in",
                p,
                "--reason",
                "r",
            ],
        );
        assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    }
    let o = text(&prova(&d, &["run", "--", "sh", "-c", &quiet]));
    assert!(o.contains("FAIL tests::flaky_clock"), "{o}");
    assert!(
        !o.contains("(calc) failed") && !o.contains("(geo) failed"),
        "{o}"
    );
}

/// Round 18 (probe M1): pytest through a container client with `-w
/// /w/tests/b` named `test_x::test_flaky` for the tests of `tests/a` and
/// `tests/b` alike. The rootdir the output prints is the container's path:
/// the client's `-v host:container` mount maps it under the repository.
#[test]
fn r18_pytest_in_a_container_workdir_is_placed_by_the_mount() {
    let d = workspace("pycontainer");
    for sub in ["a", "b"] {
        std::fs::create_dir_all(d.join("tests").join(sub)).unwrap();
        std::fs::write(d.join("tests").join(sub).join("test_x.py"), "").unwrap();
    }
    let run = |sub: &str| {
        format!(
            "{BUSY} : docker run --rm -v {d}:/w -w /w/tests/{sub} img pytest -v; printf '%s\\n' 'rootdir: /w/tests/{sub}' 'test_x.py::test_flaky FAILED [100%]' '=== 1 failed in 0.01s ==='; exit 1",
            d = d.display()
        )
    };
    let a = text(&prova(&d, &["run", "--", "sh", "-c", &run("a")]));
    assert!(a.contains("FAIL tests.a.test_x::test_flaky"), "{a}");
    let b = text(&prova(&d, &["run", "--", "sh", "-c", &run("b")]));
    assert!(b.contains("FAIL tests.b.test_x::test_flaky"), "{b}");
    assert!(b.contains("first time seen") && !b.contains("FLAKY"), "{b}");
}

/// Review of PR 115 (F): the client's mounts are read in their quoted
/// spellings (`-v "$PWD":/w`, `--mount ...,source="$(pwd)",target=/w`), and
/// only between `docker run` and the image: pytest's own `-w` after the
/// image is no workdir. Why medium: the files are found on disk.
#[test]
fn r18_container_mounts_in_quoted_spellings_place_pytest_ids() {
    let d = workspace("pyquoted");
    for sub in ["a", "b"] {
        std::fs::create_dir_all(d.join("tests").join(sub)).unwrap();
        std::fs::write(d.join("tests").join(sub).join("test_x.py"), "").unwrap();
    }
    let out =
        "'rootdir: /w/tests/b' 'test_x.py::test_flaky FAILED [100%]' '=== 1 failed in 0.01s ==='";
    for mount in [
        r#"-v "$PWD":/w"#,
        r#"--mount type=bind,source="$(pwd)",target=/w"#,
    ] {
        let cmd = format!(
            "{BUSY} : docker run --rm {mount} -w /w/tests/b img pytest -v; printf '%s\\n' {out}; exit 1"
        );
        let b = text(&prova(&d, &["run", "--", "sh", "-c", &cmd]));
        assert!(
            b.contains("FAIL tests.b.test_x::test_flaky"),
            "{mount}: {b}"
        );
    }
    // pytest's `-w` after the image is not the client's workdir.
    let cmd = format!(
        "{BUSY} : docker run --rm img pytest -v -w /w/tests/a; printf '%s\\n' 'test_x.py::test_flaky FAILED [100%]' '=== 1 failed in 0.01s ==='; exit 1"
    );
    let o = text(&prova(&d, &["run", "--", "sh", "-c", &cmd]));
    assert!(!o.contains("tests.a.test_x"), "{o}");
}

/// Review of PR 115 (G): a Go module's root package has no `/` or `.` in
/// its path (`calc`); it is still a place `quarantine --in` takes, while a
/// Rust module (`tests`) is not. Why medium: stored tests and quarantines.
#[test]
fn r18_a_go_root_package_without_a_slash_is_a_place() {
    let d = workspace("goroot");
    let go = format!(
        "{BUSY} printf '%s\\n' '=== RUN   TestFlaky' '--- PASS: TestFlaky (0.00s)' 'PASS' 'ok  \tcalc\t0.01s'"
    );
    prova(&d, &["run", "--", "sh", "-c", &go]);
    let o = prova(
        &d,
        &[
            "quarantine",
            "add",
            "TestFlaky",
            "--in",
            "calc",
            "--reason",
            "r",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let o = prova(&d, &["quarantine", "remove", "TestFlaky", "--in", "calc"]);
    assert_eq!(o.status.code(), Some(0), "remove --in: {}", text(&o));
}

/// Review of PR 115 (H): a pass of crate calc's `tests::x` on this exact
/// code is no evidence that crate geo's `tests::x` can pass: its failure is
/// not "likely FLAKY: suite passed on this exact code". Why medium: needs
/// stored runs of the same code.
#[test]
fn r18_a_pass_of_another_places_test_is_no_pass_of_this_one() {
    let d = workspace("passplace");
    let out = PathBuf::from(format!("{}.out", d.display()));
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(
        d.join("t.sh"),
        format!(
            "{BUSY}\ncat {o}/out.txt\nexit $(cat {o}/code)\n",
            o = out.display()
        ),
    )
    .unwrap();
    std::fs::write(d.join("prova.toml"), "[suite.s]\nrun = \"sh t.sh\"\n").unwrap();
    let run = |calc: &str, geo: &str, code: i32| {
        std::fs::write(
            out.join("out.txt"),
            format!(
                "    Starting 2 tests across 2 binaries\n        {calc} [   0.004s] calc tests::x\n        {geo} [   0.004s] geo tests::x\n     Summary [   0.020s] 2 tests run\n"
            ),
        )
        .unwrap();
        std::fs::write(out.join("code"), code.to_string()).unwrap();
        prova(&d, &["run", "s"])
    };
    let o = run("PASS", "SKIP", 0);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let o = text(&run("PASS", "FAIL", 100));
    assert!(o.contains("FAIL tests::x (geo)"), "{o}");
    assert!(!o.contains("likely FLAKY"), "{o}");
}

/// Round 18 (ts-vitest): "no history yet" for a budget, though the test has
/// runs, meant no duration history.
#[test]
fn r18_budget_label_says_duration_history() {
    assert!(
        prova_core::model::BudgetFrom::Default
            .describe()
            .contains("no duration history yet")
    );
}

/// Round 18 (ts MINOR, rust MINOR): a diff of two different suites listed
/// the tests the second did not print as "removed". Why medium: two stored
/// runs of two suites.
#[test]
fn r18_diff_of_different_suites_says_not_visible_and_warns() {
    let d = workspace("diffsuites");
    let one = format!(
        "{BUSY} printf '%s\\n' 'test a::t1 ... ok' 'test a::t2 ... ok' 'test result: ok. 2 passed; 0 failed; 0 ignored'"
    );
    let two = format!(
        "{BUSY} printf '%s\\n' 'test a::t1 ... ok' 'test result: ok. 1 passed; 0 failed; 0 ignored'"
    );
    let run_id = |o: &Output| {
        let t = text(o);
        let i = t.find("run ").unwrap() + 4;
        t[i..i + 8].to_owned()
    };
    let a = run_id(&prova(&d, &["run", "--", "sh", "-c", &one]));
    let b = run_id(&prova(&d, &["run", "--", "sh", "-c", &two]));
    let o = text(&prova(&d, &["diff", &a, &b]));
    assert!(o.contains("not visible in this run"), "{o}");
    assert!(!o.contains("removed ("), "{o}");
    assert!(o.contains("different suites"), "{o}");
}

/// Round 18 (ts-vitest): "environment-dependent, not flaky" from a single
/// earlier pass. Why medium: needs two runs with different environments.
#[test]
fn r18_one_earlier_pass_is_too_little_to_call_a_failure_environment_dependent() {
    let d = workspace("envone");
    let cmd = format!(
        "{BUSY} if [ -n \"$BADENV\" ]; then echo 'test calc::e ... FAILED'; exit 1; else echo 'test calc::e ... ok'; fi"
    );
    prova(&d, &["run", "--", "sh", "-c", &cmd]);
    let o = Command::new(PROVA)
        .args(["run", "--", "sh", "-c", &cmd])
        .current_dir(&d)
        .env("PROVA_HOME", format!("{}.home", d.display()))
        .env("BADENV", "1")
        .output()
        .unwrap();
    let out = text(&o);
    assert!(out.contains("FAIL calc::e"), "{out}");
    assert!(!out.contains("environment-dependent"), "{out}");
    assert!(out.contains("too little history"), "{out}");
}

/// Round 18 (ts-vitest): "this code passed in suite a" for a suite that did
/// not run the failing test at all. Why medium: three stored runs.
#[test]
fn r18_another_suite_that_never_ran_the_test_says_nothing_about_it() {
    let d = workspace("elsewhere");
    let failing = format!(
        "{BUSY} printf '%s\\n' 'test a::t1 ... FAILED' '' 'test result: FAILED. 0 passed; 1 failed; 0 ignored'; exit 101"
    );
    let other = format!(
        "{BUSY} printf '%s\\n' 'test b::other ... ok' '' 'test result: ok. 1 passed; 0 failed; 0 ignored'; exit 0"
    );
    prova(&d, &["run", "--", "sh", "-c", &failing]);
    let o = prova(&d, &["run", "--", "sh", "-c", &other]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let o = text(&prova(&d, &["run", "--", "sh", "-c", &failing]));
    assert!(o.contains("FAIL a::t1"), "{o}");
    assert!(!o.contains("this code passed in suite"), "{o}");
}

/// A name of unknown place that two known places share is none of theirs:
/// it takes no history, no FLAKY label, and no quarantine of theirs.
#[test]
fn r18_an_unknown_place_never_borrows_the_history_of_a_shared_name() {
    let d = workspace("unknownshared");
    let nextest = format!(
        "{BUSY} printf '%s\\n' '    Starting 2 tests across 2 binaries' '        PASS [   0.004s] calc tests::flaky_clock' '        FAIL [   0.004s] geo tests::flaky_clock' '     Summary [   0.020s] 2 tests run'; exit 100"
    );
    let quiet = format!(
        "{BUSY} printf '%s\\n' '.F' 'tests::flaky_clock --- FAILED' '' 'failures:' '' '---- tests::flaky_clock stdout ----' \"thread 'tests::flaky_clock' panicked at somewhere.rs:1:1:\" 'boom' '' 'failures:' '    tests::flaky_clock' '' 'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s'; exit 101"
    );
    prova(&d, &["run", "--", "sh", "-c", &nextest]);
    let o = prova(
        &d,
        &[
            "quarantine",
            "add",
            "tests::flaky_clock",
            "--in",
            "calc",
            "--reason",
            "r",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let o = prova(&d, &["run", "--", "sh", "-c", &quiet]);
    let out = text(&o);
    assert!(out.contains("first time seen"), "{out}");
    assert!(
        !out.contains("FLAKY") && !out.contains("quarantined: tests::flaky_clock (calc) failed"),
        "{out}"
    );
    assert!(!out.contains("only known flaky"), "{out}");
}
