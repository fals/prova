//! Medium tests for experiments (ADR 0007): real git worktrees, real
//! workloads, real verdicts. Workloads sleep a few ms; trials are few.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn prova(dir: &Path, args: &[&str]) -> Output {
    Command::new(PROVA)
        .args(args)
        .current_dir(dir)
        .env("PROVA_HOME", home_of(dir))
        .output()
        .expect("run prova")
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

fn sh(dir: &Path, script: &str) {
    assert!(
        Command::new("sh")
            .args(["-c", script])
            .current_dir(dir)
            .env("PROVA_HOME", home_of(dir))
            .status()
            .unwrap()
            .success(),
        "{script}"
    );
}

/// Git repo whose workload sleeps for the number of seconds in `delay`.
fn repo(name: &str, delay: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-exp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("delay"), delay).unwrap();
    std::fs::write(
        d.join("work.sh"),
        "#!/bin/sh\nsleep $(cat delay)\necho '{\"metric\":\"mem_kb\",\"value\":100}'\n",
    )
    .unwrap();
    std::fs::write(d.join(".gitignore"), ".prova/\n").unwrap();
    sh(
        &d,
        "chmod +x work.sh && git init -q && git -c user.email=t@t -c user.name=t add -A && git -c user.email=t@t -c user.name=t commit -qm base",
    );
    d
}

fn new_exp(d: &Path, extra: &[&str]) {
    let mut a = vec![
        "exp",
        "new",
        "e",
        "--hypothesis",
        "h",
        "--trials",
        "5",
        "--max-trials",
        "6",
        "--warmup",
        "0",
        "--guardrail",
        "mem_kb",
    ];
    a.extend_from_slice(extra);
    a.extend_from_slice(&["--", "./work.sh"]);
    let o = prova(d, &a);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn faster_candidate_is_accepted_and_becomes_best() {
    let d = repo("accept", "0.3");
    new_exp(&d, &[]);
    std::fs::write(d.join("delay"), "0.05").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let o = prova(&d, &["exp", "run", "e", "--json"]);
    let r = json(&o);
    assert_eq!(o.status.code(), Some(0), "{r}");
    assert_eq!(r["comparison"]["verdict"], "accept");
    assert_eq!(r["best"], "c1");
    assert_eq!(r["decisions"][0]["decision"], "keep");
    assert!(
        r["comparison"]["result"]["primary"]["improvement"]
            .as_f64()
            .unwrap()
            > 0.3
    );
    // Revalidation runs the best against the original baseline afresh.
    let v = json(&prova(&d, &["exp", "revalidate", "e", "--json"]));
    assert_eq!(v["comparison"]["candidate"], "c1");
    assert_eq!(v["comparison"]["baseline"], "baseline");
    assert_eq!(
        v["comparison"]["result"]["comparisons_in_family"], 1,
        "a revalidation adds no comparison to the Holm family"
    );
}

#[test]
fn same_code_is_not_an_improvement() {
    let d = repo("noeffect", "0.02");
    new_exp(&d, &["--min-effect", "20%"]);
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let o = prova(&d, &["exp", "run", "e", "--json"]);
    let r = json(&o);
    assert_eq!(o.status.code(), Some(1), "{r}");
    assert_eq!(r["comparison"]["verdict"], "reject");
    assert_eq!(r["best"], "baseline");
    assert_eq!(r["decisions"][0]["decision"], "discard");
}

#[test]
fn protocol_is_frozen_after_the_first_trial() {
    let d = repo("frozen", "0.01");
    new_exp(&d, &[]);
    assert!(
        prova(
            &d,
            &["exp", "protocol", "e", "--trials", "6", "--max-trials", "8"]
        )
        .status
        .success(),
        "before trials: allowed"
    );
    std::fs::write(d.join("delay"), "0.005").unwrap();
    prova(&d, &["exp", "candidate", "e"]);
    std::fs::write(d.join("delay"), "0.007").unwrap(); // edited after recording: harmless
    prova(&d, &["exp", "run", "e"]);
    let frozen = prova(&d, &["exp", "protocol", "e", "--min-effect", "0.5%"]);
    assert_eq!(frozen.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&frozen.stderr).contains("protocol frozen"));
}

#[test]
fn crashing_candidate_is_logged_as_crash() {
    let d = repo("crash", "0.01");
    // The workload script is a harness file: allow changing it here.
    new_exp(&d, &["--change", "**"]);
    std::fs::write(d.join("work.sh"), "#!/bin/sh\nexit 3\n").unwrap();
    prova(&d, &["exp", "candidate", "e"]);
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["status"], "failed");
    assert_eq!(r["decisions"][0]["decision"], "crash");
}

/// Isolated prova home for a test workspace, outside it (ADR 0014).
fn home_of(dir: &Path) -> PathBuf {
    PathBuf::from(format!("{}.home", dir.display()))
}

/// Repo for a non-speed experiment: the workload reports `steps`; the
/// specification says the answer must be 42, and the oracle checks it.
fn spec_repo(name: &str) -> PathBuf {
    spec_repo_with(
        name,
        "test \"$(cat answer)\" = 42 || { echo \"answer $(cat answer), spec says 42\"; exit 1; }",
    )
}

fn spec_repo_with(name: &str, oracle: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-exp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("steps"), "100").unwrap();
    std::fs::write(d.join("answer"), "42").unwrap();
    std::fs::write(d.join("spec.txt"), "answer = 42\n").unwrap();
    std::fs::write(
        d.join("work.sh"),
        "#!/bin/sh\necho \"{\\\"metric\\\":\\\"steps\\\",\\\"value\\\":$(cat steps)}\"\n",
    )
    .unwrap();
    std::fs::write(d.join(".gitignore"), ".prova/\n").unwrap();
    sh(
        &d,
        "chmod +x work.sh && git init -q && git -c user.email=t@t -c user.name=t add -A && git -c user.email=t@t -c user.name=t commit -qm base",
    );
    let o = prova(
        &d,
        &[
            "exp",
            "new",
            "e",
            "--hypothesis",
            "fewer steps, same answer",
            "--metric",
            "steps",
            "--better",
            "lower",
            "--min-effect",
            "10%",
            "--trials",
            "5",
            "--max-trials",
            "5",
            "--warmup",
            "0",
            "--spec",
            "spec.txt",
            "--oracle",
            oracle,
            "--",
            "./work.sh",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    d
}

#[test]
fn deterministic_metric_under_a_specification_is_accepted() {
    let d = spec_repo("spec-accept");
    std::fs::write(d.join("steps"), "60").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["verdict"], "accept", "{r}");
    assert_eq!(
        r["comparison"]["result"]["primary"]["candidate_median"],
        60.0
    );
    // ADR 0018: the verdict says the oracle ran and held on both arms.
    let spec = r["comparison"]["result"]["specification"].as_str().unwrap();
    assert!(spec.starts_with("holds on both arms"), "{spec}");
    // A silent oracle is judged by its exit code only, and says so.
    assert!(spec.contains("exit code only"), "{spec}");
    // Its log is kept even though it passed, so the verdict can be audited.
    assert!(
        prova(&d, &["exp", "log", "e", "oracle-candidate"])
            .status
            .success()
    );
}

/// Round 7: a candidate added code that made the oracle exit 0 without
/// running its checks, and was accepted with "holds on both arms".
#[test]
fn an_oracle_whose_checks_were_skipped_does_not_hold() {
    let d = spec_repo_with(
        "spec-skipped",
        "[ -f skip ] && exit 0; test \"$(cat answer)\" = 42 && echo 'test spec::answer ... ok'",
    );
    std::fs::write(d.join("steps"), "10").unwrap();
    std::fs::write(d.join("answer"), "41").unwrap();
    std::fs::write(d.join("skip"), "").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["verdict"], "reject", "{r}");
    assert_eq!(r["comparison"]["candidate_trials"], 0, "never measured");
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("violates the specification") && reason.contains("spec::answer"),
        "{reason}"
    );
}

#[test]
fn an_oracle_that_shows_its_tests_holds_with_their_count() {
    let d = spec_repo_with(
        "spec-counted",
        "test \"$(cat answer)\" = 42 && echo 'test spec::answer ... ok'",
    );
    std::fs::write(d.join("steps"), "60").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["verdict"], "accept", "{r}");
    let spec = r["comparison"]["result"]["specification"].as_str().unwrap();
    assert!(spec.contains("the same 1 test(s) passed on each"), "{spec}");
}

#[test]
fn candidate_violating_the_specification_is_rejected_unmeasured() {
    let d = spec_repo("spec-violation");
    std::fs::write(d.join("steps"), "10").unwrap();
    std::fs::write(d.join("answer"), "41").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let o = prova(&d, &["exp", "run", "e", "--json"]);
    let r = json(&o);
    assert_eq!(o.status.code(), Some(1), "{r}");
    assert_eq!(r["comparison"]["verdict"], "reject");
    assert_eq!(r["comparison"]["candidate_trials"], 0, "never measured");
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
    assert!(reason.contains("violates the specification"), "{reason}");
    assert!(
        reason.contains("prova exp log e oracle-candidate"),
        "{reason}"
    );
    // The oracle's log is kept in the store; the log directory is gone.
    let log = prova(&d, &["exp", "log", "e", "oracle-candidate"]);
    let log = String::from_utf8_lossy(&log.stdout).into_owned();
    assert!(
        log.starts_with("== oracle-candidate\nanswer 41, spec says 42"),
        "{log}"
    );
    let store = home_of(&d);
    let leftovers = std::process::Command::new("find")
        .arg(&store)
        .args(["-path", "*/experiments/*", "-o", "-path", "*/worktrees/*"])
        .output()
        .unwrap();
    assert!(
        leftovers.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&leftovers.stdout)
    );
}

#[test]
fn the_specification_cannot_move() {
    let d = spec_repo("spec-moved");
    std::fs::write(d.join("steps"), "10").unwrap();
    std::fs::write(d.join("spec.txt"), "answer = anything\n").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["verdict"], "reject", "{r}");
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
    assert!(reason.contains("spec.txt differs"), "{reason}");
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}");
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn a_broken_edit_after_an_accept_is_rejected_and_the_best_still_runs_as_recorded() {
    // Why medium: needs real worktrees and a persisted best across comparisons.
    let d = spec_repo("snap-broken");
    std::fs::write(d.join("steps"), "60").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["best"], "c1", "{r}");
    // A deliberately broken edit, recorded as a new candidate.
    std::fs::write(d.join("steps"), "10").unwrap();
    std::fs::write(d.join("answer"), "41").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let o = prova(&d, &["exp", "run", "e", "--json"]);
    let r = json(&o);
    assert_eq!(
        r["comparison"]["status"], "decided",
        "best must hold its recorded spec: {r}"
    );
    assert_eq!(r["comparison"]["verdict"], "reject", "{r}");
    assert_eq!(r["comparison"]["candidate_trials"], 0, "{r}");
    assert_eq!(r["best"], "c1");
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
    assert!(reason.contains("violates the specification"), "{reason}");
}

#[test]
fn edits_after_recording_do_not_change_what_is_measured() {
    // A gap far above process start-up jitter (shared macOS runners).
    let d = repo("snap-edit", "0.3");
    new_exp(&d, &[]);
    std::fs::write(d.join("delay"), "0.05").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    std::fs::write(d.join("delay"), "0.5").unwrap(); // edited after recording
    let o = prova(&d, &["exp", "run", "e", "--json"]);
    let r = json(&o);
    assert_eq!(o.status.code(), Some(0), "{r}");
    assert_eq!(r["comparison"]["verdict"], "accept", "{r}");
    // The live tree is never measured: it still holds the edit.
    assert_eq!(std::fs::read_to_string(d.join("delay")).unwrap(), "0.5");
}

#[test]
fn recording_a_candidate_leaves_the_users_git_state_alone() {
    let d = repo("snap-state", "0.01");
    new_exp(&d, &[]);
    std::fs::write(d.join("delay"), "0.005").unwrap();
    sh(
        &d,
        "git -c user.email=t@t -c user.name=t stash push -q -m mine",
    );
    std::fs::write(d.join("delay"), "0.004").unwrap();
    std::fs::write(d.join("new.txt"), "untracked\n").unwrap();
    std::fs::write(d.join("staged.txt"), "staged\n").unwrap();
    sh(&d, "git add staged.txt");
    let state = |d: &Path| {
        (
            git_out(d, &["rev-parse", "HEAD"]),
            git_out(d, &["symbolic-ref", "HEAD"]),
            git_out(d, &["stash", "list"]),
            git_out(d, &["status", "--porcelain=v1"]),
            git_out(d, &["diff", "--cached"]),
        )
    };
    let before = state(&d);
    assert!(before.2.contains("mine"), "stash entry exists");
    assert!(
        prova(&d, &["exp", "candidate", "e", "--label", "idea"])
            .status
            .success()
    );
    assert_eq!(
        state(&d),
        before,
        "HEAD, branch, stash, index and tree untouched"
    );
    // The snapshot is a commit kept alive by a ref, holds untracked files, no .prova.
    let sha = git_out(&d, &["rev-parse", "refs/prova/candidates/e/idea"]);
    let sha = sha.trim();
    assert_eq!(git_out(&d, &["show", &format!("{sha}:delay")]), "0.004");
    assert_eq!(
        git_out(&d, &["show", &format!("{sha}:new.txt")]),
        "untracked\n"
    );
    let files = git_out(&d, &["ls-tree", "-r", "--name-only", sha]);
    assert!(!files.lines().any(|f| f.starts_with(".prova")), "{files}");
    assert_eq!(
        git_out(&d, &["rev-parse", &format!("{sha}^")]).trim(),
        before.0.trim()
    );
}

#[test]
fn candidates_work_in_a_repository_without_commits() {
    let d = std::env::temp_dir().join(format!("prova-exp-snap-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("delay"), "0.3").unwrap();
    std::fs::write(
        d.join("work.sh"),
        "#!/bin/sh\nsleep $(cat delay)\necho '{\"metric\":\"mem_kb\",\"value\":100}'\n",
    )
    .unwrap();
    sh(&d, "chmod +x work.sh && git init -q");
    new_exp(&d, &[]);
    std::fs::write(d.join("delay"), "0.05").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["verdict"], "accept", "{r}");
}

/// Round 5: `prova exp protocol --spec X` reset the metric, the effect and the
/// trials to their defaults and dropped the oracle, silently.
#[test]
fn a_protocol_change_keeps_what_it_does_not_name() {
    let d = spec_repo("protocol-merge");
    std::fs::write(d.join("extra.txt"), "x").unwrap();
    sh(
        &d,
        "git -c user.email=t@t -c user.name=t add -A && git -c user.email=t@t -c user.name=t commit -qm extra",
    );
    let o = prova(&d, &["exp", "protocol", "e", "--spec", "extra.txt"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let shown = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(
        shown.contains("metric steps") && shown.contains("oracle `"),
        "{shown}"
    );
    let r = json(&prova(&d, &["exp", "status", "e", "--json"]));
    let p = &r["protocol"];
    assert_eq!(p["metric"], "steps");
    assert_eq!(p["trials"], 5);
    assert!(p["oracle"].as_str().unwrap().contains("cat answer"));
    assert_eq!(p["specs"].as_array().unwrap().len(), 2);
}

/// Round 7: a prova home inside the repository, ignored by git, made the
/// candidate snapshot fail (`git add` refuses to name an ignored path).
#[test]
fn a_store_ignored_inside_the_repository_does_not_block_candidates() {
    let d = repo("home-inside", "0.01");
    std::fs::write(d.join(".gitignore"), ".prova/\n.ph/\n").unwrap();
    let inside = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_prova"))
            .args(args)
            .current_dir(&d)
            .env("PROVA_HOME", d.join(".ph"))
            .output()
            .unwrap()
    };
    let o = inside(&[
        "exp",
        "new",
        "e",
        "--hypothesis",
        "h",
        "--metric",
        "mem_kb",
        "--",
        "./work.sh",
    ]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = inside(&["exp", "candidate", "e"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// Round 8 red team: a candidate may not change the files that decide the
/// verdict (tests, benchmarks, build files) unless the protocol allows it.
#[test]
fn a_candidate_that_changes_the_harness_is_rejected_unmeasured() {
    let d = repo("scope", "0.01");
    new_exp(&d, &[]);
    std::fs::write(
        d.join("work.sh"),
        "#!/bin/sh\necho '{\"metric\":\"mem_kb\",\"value\":1}'\n",
    )
    .unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["verdict"], "reject", "{r}");
    assert_eq!(r["comparison"]["candidate_trials"], 0, "never measured");
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("out of scope") && reason.contains("work.sh"),
        "{reason}"
    );
}

/// Round 8 red team: with `--change`, only those paths may change.
#[test]
fn a_change_scope_limits_what_a_candidate_may_touch() {
    let d = repo("scope-explicit", "0.01");
    std::fs::write(d.join("other"), "1").unwrap();
    sh(
        &d,
        "git -c user.email=t@t -c user.name=t add -A && git -c user.email=t@t -c user.name=t commit -qm other",
    );
    new_exp(&d, &["--change", "delay", "--change-only"]);
    std::fs::write(d.join("other"), "2").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap_or("");
    assert!(
        reason.contains("outside the protocol's --change-only scope, e.g. other"),
        "{r}"
    );
}

/// Round 8 red team: code under test printed extra benchmark lines, and
/// the median of real and fake lines was the fake value.
#[test]
fn extra_metric_lines_void_the_comparison() {
    let d = std::env::temp_dir().join(format!("prova-exp-forged-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("reps"), "1").unwrap();
    std::fs::write(
        d.join("work.sh"),
        "#!/bin/sh\nfor i in $(seq $(cat reps)); do echo '{\"metric\":\"t\",\"value\":5}'; done\n",
    )
    .unwrap();
    sh(
        &d,
        "chmod +x work.sh && git init -q && git -c user.email=t@t -c user.name=t add -A && git -c user.email=t@t -c user.name=t commit -qm base",
    );
    let o = prova(
        &d,
        &[
            "exp",
            "new",
            "e",
            "--hypothesis",
            "h",
            "--metric",
            "t",
            "--trials",
            "5",
            "--warmup",
            "0",
            "--",
            "./work.sh",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    std::fs::write(d.join("reps"), "3").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["status"], "failed", "{r}");
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("reported t 3x per trial, the baseline's 1x"),
        "{reason}"
    );
    // Round 15: the trial output is kept, and the reason says how to read it.
    assert!(reason.contains("prova exp log e "), "{reason}");
    let log = prova(&d, &["exp", "log", "e"]);
    let text = String::from_utf8_lossy(&log.stdout);
    assert!(text.contains("== trial"), "{text}");
}

/// Round 8 red team: a process escaping the trial's group (setsid) kept
/// running into the other arm's trials and skewed them.
#[test]
fn a_trial_that_leaves_a_process_behind_voids_the_comparison() {
    let d = std::env::temp_dir().join(format!("prova-exp-leak-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("work.sh"),
        "#!/bin/sh\ncommand -v setsid >/dev/null && S=setsid\nif [ -f leak ]; then $S sleep 37.125 >/dev/null 2>&1 < /dev/null & fi\necho '{\"metric\":\"t\",\"value\":5}'\n",
    )
    .unwrap();
    sh(
        &d,
        "chmod +x work.sh && git init -q && git -c user.email=t@t -c user.name=t add -A && git -c user.email=t@t -c user.name=t commit -qm base",
    );
    let o = prova(
        &d,
        &[
            "exp",
            "new",
            "e",
            "--hypothesis",
            "h",
            "--metric",
            "t",
            "--trials",
            "5",
            "--warmup",
            "0",
            "--",
            "./work.sh",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    std::fs::write(d.join("leak"), "").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["status"], "failed", "{r}");
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
    assert!(
        // The leftover may not have exec'd `sleep` yet (sh, setsid).
        reason.contains("left process"),
        "{reason}"
    );
}

/// Round 8: a spec edited but not committed was frozen as edited, and every
/// comparison then said "baseline violates the specification".
#[test]
fn an_uncommitted_spec_is_refused_at_creation() {
    let d = repo("dirty-spec", "0.01");
    std::fs::write(d.join("spec.txt"), "answer = 42\n").unwrap();
    sh(
        &d,
        "git -c user.email=t@t -c user.name=t add -A && git -c user.email=t@t -c user.name=t commit -qm spec",
    );
    std::fs::write(d.join("spec.txt"), "answer = anything\n").unwrap();
    let o = prova(
        &d,
        &[
            "exp",
            "new",
            "e",
            "--hypothesis",
            "h",
            "--spec",
            "spec.txt",
            "--",
            "./work.sh",
        ],
    );
    assert_eq!(o.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&o.stderr).contains("spec.txt differs from the baseline HEAD"));
}

/// Round 8: after an ACCEPT found wrong, the best can be set back, and the
/// decision log says so.
#[test]
fn the_best_can_be_reset_with_a_reason() {
    let d = repo("reset-best", "0.3");
    new_exp(&d, &[]);
    std::fs::write(d.join("delay"), "0.05").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["best"], "c1", "{r}");
    let o = prova(
        &d,
        &["exp", "best", "e", "baseline", "--reason", "cached results"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let s = json(&prova(&d, &["exp", "status", "e", "--json"]));
    assert_eq!(s["best"], "baseline");
    let last = s["decisions"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["decision"], "reset", "{last}");
}

/// Round 9 red team: code that rewrote tracked files at run time (its own
/// frozen reference table, the other arm's sources) was accepted.
#[test]
fn a_workload_that_rewrites_tracked_files_voids_the_comparison() {
    let d = repo("tamper", "0.01");
    std::fs::write(d.join("table"), "1\n").unwrap();
    sh(
        &d,
        "git -c user.email=t@t -c user.name=t add -A && git -c user.email=t@t -c user.name=t commit -qm table",
    );
    new_exp(&d, &["--change", "**"]);
    std::fs::write(
        d.join("work.sh"),
        "#!/bin/sh\nchmod u+w table 2>/dev/null; echo 2 >> table\necho '{\"metric\":\"mem_kb\",\"value\":100}'\n",
    )
    .unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["status"], "failed", "{r}");
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("changed tracked files") && reason.contains("table"),
        "{reason}"
    );
}

/// Round 9 red team: a no-op beat a discarded slower variant with
/// `--against` and became best; `exp best` took any variant.
#[test]
fn only_the_baseline_or_a_kept_variant_can_be_the_reference() {
    let d = repo("against", "0.01");
    new_exp(&d, &[]);
    std::fs::write(d.join("delay"), "0.08").unwrap();
    assert!(
        prova(&d, &["exp", "candidate", "e", "--label", "slow"])
            .status
            .success()
    );
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["verdict"], "reject", "{r}");
    std::fs::write(d.join("delay"), "0.01").unwrap();
    assert!(
        prova(&d, &["exp", "candidate", "e", "--label", "noop"])
            .status
            .success()
    );
    let o = prova(&d, &["exp", "run", "e", "--against", "slow"]);
    assert_eq!(o.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&o.stderr).contains("cannot be the reference"));
    let o = prova(&d, &["exp", "best", "e", "slow", "--reason", "x"]);
    assert_eq!(o.status.code(), Some(4));
}

/// Round 9 red team: a trial's code rewrote the baseline's samples through
/// PROVA_HOME; trials no longer see it.
#[test]
fn trials_do_not_see_prova_home() {
    let d = std::env::temp_dir().join(format!("prova-exp-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("work.sh"),
        "#!/bin/sh\n[ -n \"$PROVA_HOME\" ] && exit 3\necho '{\"metric\":\"t\",\"value\":5}'\n",
    )
    .unwrap();
    sh(
        &d,
        "chmod +x work.sh && git init -q && git -c user.email=t@t -c user.name=t add -A && git -c user.email=t@t -c user.name=t commit -qm base",
    );
    let o = prova(
        &d,
        &[
            "exp",
            "new",
            "e",
            "--hypothesis",
            "h",
            "--metric",
            "t",
            "--trials",
            "5",
            "--warmup",
            "0",
            "--oracle",
            "[ -z \"$PROVA_HOME\" ]",
            "--",
            "./work.sh",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    std::fs::write(d.join("x"), "").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["status"], "decided", "{r}");
}

/// Round 10: `--change` replaced the default scope; now it adds to it.
#[test]
fn change_allows_a_build_file_on_top_of_the_default_scope() {
    let d = repo("change-adds", "0.01");
    std::fs::write(d.join("Makefile"), "all:\n").unwrap();
    sh(
        &d,
        "git -c user.email=t@t -c user.name=t add -A && git -c user.email=t@t -c user.name=t commit -qm mk",
    );
    new_exp(&d, &["--change", "Makefile"]);
    std::fs::write(d.join("Makefile"), "all:\n\ttrue\n").unwrap();
    std::fs::write(d.join("delay"), "0.012").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap_or("");
    assert!(!reason.contains("out of scope"), "{r}");
}

/// Round 10: a comparison that failed at its oracle froze the protocol, so
/// fixing the oracle needed a new experiment.
#[test]
fn the_protocol_freezes_at_the_first_trial_not_at_a_failed_oracle() {
    let d = repo("freeze", "0.01");
    new_exp(&d, &["--oracle", "false"]);
    std::fs::write(d.join("delay"), "0.012").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    prova(&d, &["exp", "run", "e"]);
    let o = prova(&d, &["exp", "protocol", "e", "--oracle", "true"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// Round 10 red team: Python's user site-packages crossed arms; trials run
/// without it.
#[test]
fn trials_run_without_the_python_user_site() {
    let d = std::env::temp_dir().join(format!("prova-exp-usersite-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("work.sh"),
        "#!/bin/sh\n[ \"$PYTHONNOUSERSITE\" = 1 ] || exit 3\necho '{\"metric\":\"t\",\"value\":5}'\n",
    )
    .unwrap();
    sh(
        &d,
        "chmod +x work.sh && git init -q && git -c user.email=t@t -c user.name=t add -A && git -c user.email=t@t -c user.name=t commit -qm base",
    );
    let o = prova(
        &d,
        &[
            "exp",
            "new",
            "e",
            "--hypothesis",
            "h",
            "--metric",
            "t",
            "--trials",
            "5",
            "--warmup",
            "0",
            "--",
            "./work.sh",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    std::fs::write(d.join("x"), "").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["status"], "decided", "{r}");
}

/// Round 11 red team: the shared Go build cache let a candidate patch the
/// baseline's compiled test. Each arm gets its own caches; the session's
/// environment is otherwise the workload's (ADR 0037: no secret stripping).
#[test]
fn trials_get_private_caches_and_the_sessions_environment() {
    let d = std::env::temp_dir().join(format!("prova-exp-caches-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("work.sh"),
        "#!/bin/sh\ncase \"$GOCACHE\" in */prova-scratch-*/arm-*) ;; *) exit 3;; esac\n[ \"$MY_API_TOKEN\" = s3cret ] || exit 4\necho '{\"metric\":\"t\",\"value\":5}'\n",
    )
    .unwrap();
    sh(
        &d,
        "chmod +x work.sh && git init -q && git -c user.email=t@t -c user.name=t add -A && git -c user.email=t@t -c user.name=t commit -qm base",
    );
    let run = |args: &[&str]| {
        Command::new(PROVA)
            .args(args)
            .current_dir(&d)
            .env("PROVA_HOME", home_of(&d))
            .env("MY_API_TOKEN", "s3cret")
            .output()
            .unwrap()
    };
    let o = run(&[
        "exp",
        "new",
        "e",
        "--hypothesis",
        "h",
        "--metric",
        "t",
        "--trials",
        "5",
        "--warmup",
        "0",
        "--",
        "./work.sh",
    ]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    std::fs::write(d.join("x"), "").unwrap();
    assert!(run(&["exp", "candidate", "e"]).status.success());
    let r = json(&run(&["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["status"], "decided", "{r}");
}

/// Round 11 red team: editing prova.db turned a REJECT into an ACCEPT;
/// `exp status` re-derives every ACCEPT from the samples.
#[test]
fn an_edited_verdict_shows_in_status() {
    // Clearly slower, so the honest verdict is never an accept.
    let d = repo("edited", "0.05");
    new_exp(&d, &[]);
    std::fs::write(d.join("delay"), "0.1").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_ne!(r["comparison"]["verdict"], "accept", "{r}");
    let db = std::fs::read_dir(home_of(&d).join("repos"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("prova.db");
    let ok = Command::new("sqlite3")
        .arg(&db)
        .arg("UPDATE comparisons SET verdict = 'accept'; UPDATE experiments SET best = 'c1';")
        .status();
    if ok.is_err() {
        return; // no sqlite3 here: nothing to edit with
    }
    let s = String::from_utf8_lossy(&prova(&d, &["exp", "status", "e"]).stdout).into_owned();
    assert!(s.contains("the record was changed"), "{s}");
}

/// Usability round: an oracle that cannot run (command not found) said the
/// baseline "violates the specification". It is an infrastructure failure,
/// for either arm, and never a REJECT.
#[test]
fn an_oracle_that_cannot_run_is_not_a_specification_violation() {
    let d = spec_repo_with("oracle-missing", "prova_no_such_tool_xyz spec.txt");
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let o = prova(&d, &["exp", "run", "e", "--json"]);
    let r = json(&o);
    assert_eq!(r["comparison"]["status"], "failed", "{r}");
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
    assert!(!reason.contains("violates the specification"), "{reason}");
    assert!(
        reason.contains("could not run")
            && reason.contains("exit 127")
            && reason.contains("prova exp log e oracle-baseline"),
        "{reason}"
    );
}

#[test]
fn a_candidate_whose_oracle_cannot_run_is_not_rejected() {
    let d = spec_repo_with(
        "oracle-missing-cand",
        "[ -f broken ] && exec prova_no_such_tool_xyz; test \"$(cat answer)\" = 42",
    );
    std::fs::write(d.join("steps"), "10").unwrap();
    std::fs::write(d.join("broken"), "").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["status"], "failed", "{r}");
    assert_ne!(r["comparison"]["verdict"], "reject", "{r}");
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
    assert!(
        !reason.contains("violates the specification") && reason.contains("oracle-candidate"),
        "{reason}"
    );
}

/// The decisive line of the oracle's output is in the reason itself.
#[test]
fn a_rejection_for_the_oracle_quotes_its_last_line() {
    let d = spec_repo("oracle-last-line");
    std::fs::write(d.join("steps"), "10").unwrap();
    std::fs::write(d.join("answer"), "41").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("violates the specification: answer 41, spec says 42 (full log: prova exp log e oracle-candidate)"),
        "{reason}"
    );
}

/// `--max-wait` bounds the foreground wait of `exp run`.
#[test]
fn exp_run_accepts_max_wait_and_hands_over_the_wait() {
    let d = repo("max-wait", "0.5");
    new_exp(&d, &[]);
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let o = prova(&d, &["exp", "run", "e", "--max-wait", "1s"]);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(
        o.status.code(),
        Some(5),
        "{out}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(out.contains("next: prova exp wait e --max-wait"), "{out}");
    // Round 18: the timed-out wait printed `next:` twice.
    assert_eq!(out.matches("next:").count(), 1, "{out}");
    assert!(prova(&d, &["exp", "cancel", "e"]).status.success());
}

/// After a later REJECT the status leads with the best and exits 0.
#[test]
fn status_leads_with_the_best_after_a_later_reject() {
    let d = repo("status-best", "0.3");
    new_exp(&d, &["--min-effect", "20%"]);
    std::fs::write(d.join("delay"), "0.05").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    assert_eq!(prova(&d, &["exp", "run", "e"]).status.code(), Some(0));
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let o = prova(&d, &["exp", "run", "e"]);
    assert_eq!(o.status.code(), Some(1), "a run's own REJECT stays 1");
    let o = prova(&d, &["exp", "status", "e"]);
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(o.status.code(), Some(0), "{out}");
    let best = out
        .find("best: c1 (ACCEPT")
        .unwrap_or_else(|| panic!("{out}"));
    let latest = out.find("REJECT  c2").unwrap_or_else(|| panic!("{out}"));
    assert!(best < latest, "{out}");
    // Holm: what the family holds is said.
    assert!(
        out.contains("comparison(s) in this experiment's family for this metric, Holm"),
        "{out}"
    );
}

/// Usability round 13: files the oracle command reads that no `--spec`
/// freezes are named at `exp new`, with the flag to add them (medium: needs
/// a git repository and the binary).
#[test]
fn exp_new_warns_about_oracle_files_that_are_not_frozen() {
    let d = spec_repo_with(
        "oracle-unfrozen",
        "test -f work.sh && test \"$(cat answer)\" = 42",
    );
    let o = prova(
        &d,
        &[
            "exp",
            "new",
            "e2",
            "--hypothesis",
            "h",
            "--spec",
            "spec.txt",
            "--oracle",
            "test -f work.sh && test -f spec.txt",
            "--",
            "./work.sh",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("--spec work.sh"), "{err}");
    assert!(!err.contains("--spec spec.txt"), "{err}");
}

/// `exp protocol --no-spec` removes a frozen specification file.
#[test]
fn exp_protocol_can_remove_a_specification_file() {
    let d = spec_repo("no-spec");
    let o = prova(&d, &["exp", "protocol", "e", "--no-spec", "spec.txt"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let again = prova(&d, &["exp", "protocol", "e", "--no-spec", "spec.txt"]);
    assert!(!again.status.success());
    let err = String::from_utf8_lossy(&again.stderr);
    assert!(err.contains("not a specification file"), "{err}");
}

/// A REJECT quotes the line that matters, not the runner's trailer.
#[test]
fn a_rejection_skips_generic_trailers_in_the_oracle_log() {
    let d = spec_repo_with(
        "oracle-trailer",
        "test \"$(cat answer)\" = 42 || { echo \"answer $(cat answer), spec says 42\"; echo 'exit status 1'; exit 1; }",
    );
    std::fs::write(d.join("steps"), "10").unwrap();
    std::fs::write(d.join("answer"), "41").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
    assert!(reason.contains("answer 41, spec says 42"), "{reason}");
}

/// Usability round 13: the `next:` of `exp metrics` is a command that parses
/// (it names the required --hypothesis as a placeholder to fill in).
#[test]
fn exp_metrics_next_line_names_the_hypothesis() {
    let d = spec_repo("metrics-next");
    let o = prova(&d, &["exp", "metrics", "--", "./work.sh"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("next: prova exp new <name> --hypothesis"),
        "{out}"
    );
}

/// Round 14: a verdict with no correctness gate says so, and its `next:`
/// does not send the agent off to keep unchecked code.
#[test]
fn a_verdict_without_oracle_or_spec_carries_a_caveat_and_a_careful_next() {
    let d = repo("nogate", "0.3");
    new_exp(&d, &[]);
    std::fs::write(d.join("delay"), "0.05").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let o = prova(&d, &["exp", "run", "e"]);
    let t = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(
        t.contains(
            "caveat: no oracle or spec in the protocol: correctness was not checked, only speed"
        ),
        "{t}"
    );
    assert!(
        t.contains("next: keep it once its correctness is checked (tests, or an --oracle)"),
        "{t}"
    );
    let r = json(&prova(&d, &["exp", "status", "e", "--json"]));
    let cav = r["comparison"]["result"]["caveats"].to_string();
    assert!(cav.contains("no oracle or spec"), "{cav}");
}

/// Round 14: an ACCEPT against something other than the best says, in the
/// `next:` line, that the best is unchanged; the Holm family and the
/// discard log name what they count and compare.
#[test]
fn accept_against_baseline_says_the_best_is_unchanged_and_logs_what_it_compared() {
    // Delays long against process start noise (macOS CI: a 20 ms arm
    // measured 43 ms, then 92 ms, and the comparison was void), with wide
    // gaps (arm64 CI: 0.2 s against 0.3 s was inconclusive).
    let d = repo("notbest", "0.6");
    new_exp(&d, &[]);
    std::fs::write(d.join("delay"), "0.1").unwrap();
    assert!(
        prova(&d, &["exp", "candidate", "e", "--label", "fast"])
            .status
            .success()
    );
    assert_eq!(
        json(&prova(&d, &["exp", "run", "e", "--json"]))["best"],
        "fast"
    );
    std::fs::write(d.join("delay"), "0.25").unwrap();
    assert!(
        prova(&d, &["exp", "candidate", "e", "--label", "mid"])
            .status
            .success()
    );
    let o = prova(&d, &["exp", "run", "e", "--against", "baseline"]);
    let t = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(t.contains("ACCEPT"), "{t}");
    assert!(
        t.contains("accepted against baseline, not against the best: the best is still fast"),
        "{t}"
    );
    assert!(!t.contains("on this baseline and metric"), "{t}");
    assert!(
        t.contains("in this experiment's family for this metric"),
        "{t}"
    );
    // A discard names its reference.
    std::fs::write(d.join("delay"), "1.2").unwrap();
    assert!(
        prova(&d, &["exp", "candidate", "e", "--label", "slow"])
            .status
            .success()
    );
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    let last = r["decisions"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["decision"], "discard", "{r}");
    assert!(
        last["reason"].as_str().unwrap().contains("vs fast"),
        "{last}"
    );
}

/// Round 15: an experiment that failed before any trial measured nothing,
/// so it does not count toward "other experiments on this baseline"; and
/// the protocol line says which files are protected.
#[test]
fn unmeasured_experiments_do_not_count_as_other_experiments() {
    let d = repo("caveat-count", "0.3");
    let o = prova(
        &d,
        &[
            "exp",
            "new",
            "e0",
            "--hypothesis",
            "h",
            "--metric",
            "mem_kb",
            "--oracle",
            "false",
            "--",
            "./work.sh",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let st = prova(&d, &["exp", "status", "e0"]);
    let shown = String::from_utf8_lossy(&st.stdout).into_owned();
    assert!(
        shown.contains("Cargo.toml") && shown.contains("Makefile"),
        "{shown}"
    );
    assert!(prova(&d, &["exp", "candidate", "e0"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e0", "--json"]));
    assert_eq!(r["comparison"]["status"], "failed", "{r}");
    new_exp(&d, &["--metric", "mem_kb"]);
    std::fs::write(d.join("delay"), "0.05").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    let caveats = r["comparison"]["result"]["caveats"].to_string();
    assert!(!caveats.contains("other experiment"), "{r}");
}

/// Round 16: the oracle's build ran out of disk quota in the candidate's
/// checkout; the correct candidate was REJECTed as violating the spec.
#[test]
fn an_oracle_out_of_room_is_not_a_specification_violation() {
    let d = spec_repo_with(
        "oracle-quota",
        "[ -f broken ] && { echo 'error: failed to write target/deps/x.rmeta: Disk quota exceeded (os error 122)'; exit 101; }; test \"$(cat answer)\" = 42",
    );
    std::fs::write(d.join("steps"), "10").unwrap();
    std::fs::write(d.join("broken"), "").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_eq!(r["comparison"]["status"], "failed", "{r}");
    assert_ne!(r["comparison"]["verdict"], "reject", "{r}");
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("ran out of room") && reason.contains("Disk quota"),
        "{reason}"
    );
}

/// Round 16: a 527288x "improvement" (the candidate special-cased the
/// benchmark's input) ended its long reason with the warning, and `next:`
/// said keep it.
#[test]
fn an_implausibly_large_win_is_warned_about_on_its_own_line() {
    let d = spec_repo_with("huge", "test \"$(cat answer)\" = 42");
    std::fs::write(d.join("steps"), "0.05").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let o = prova(&d, &["exp", "run", "e"]);
    let t = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(t.contains("ACCEPT"), "{t}");
    assert!(
        t.lines()
            .any(|l| l.starts_with("warning: a change this large is rarely real")),
        "{t}"
    );
    assert!(
        t.contains("next: read the candidate's diff before trusting this"),
        "{t}"
    );
}

/// A repository whose candidate (the file `cand`) cuts the workload's steps
/// from 100 to 40 while `KNOB` is set: accepted as `c1` on the first run.
/// `SLOW` triples both arms (a throttled machine); `REWRITE_CAND` and
/// `REWRITE_BASE` make that arm's workload rewrite a tracked file.
fn knob_accepted(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-exp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("work.sh"),
        "#!/bin/sh\nv=100\nif [ -f cand ] && [ -n \"$KNOB\" ]; then v=40; fi\nif [ -n \"$SLOW\" ]; then v=$((v*3)); fi\nif { [ -n \"$REWRITE_CAND\" ] && [ -f cand ]; } || { [ -n \"$REWRITE_BASE\" ] && [ ! -f cand ]; }; then chmod u+w table 2>/dev/null; echo 2 >> table; fi\necho \"{\\\"metric\\\":\\\"steps\\\",\\\"value\\\":$v}\"\n",
    )
    .unwrap();
    std::fs::write(d.join("table"), "1\n").unwrap();
    std::fs::write(d.join(".gitignore"), ".prova/\n").unwrap();
    sh(
        &d,
        "chmod +x work.sh && git init -q && git -c user.email=t@t -c user.name=t add -A && git -c user.email=t@t -c user.name=t commit -qm base",
    );
    let o = prova(
        &d,
        &[
            "exp",
            "new",
            "e",
            "--hypothesis",
            "h",
            "--metric",
            "steps",
            "--better",
            "lower",
            "--min-effect",
            "10%",
            "--trials",
            "5",
            "--max-trials",
            "5",
            "--warmup",
            "0",
            "--",
            "./work.sh",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    std::fs::write(d.join("cand"), "x").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova_env(
        &d,
        &["exp", "run", "e", "--json"],
        &[("KNOB", "1")],
    ));
    assert_eq!(r["comparison"]["verdict"], "accept", "{r}");
    assert_eq!(r["best"], "c1", "{r}");
    d
}

fn prova_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(PROVA);
    c.args(args)
        .current_dir(dir)
        .env("PROVA_HOME", home_of(dir));
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

/// ADR 0035 section 4 (round 18 B8): a candidate that is fast only under a
/// condition of the first run was accepted; the revalidation measured the
/// ordinary result, the candidate's own median moved 2.5x while the
/// baseline's did not, and the accept survived. A drift that is the
/// candidate's own withdraws the accept.
/// Medium: needs a repository, the binary and two comparisons.
#[test]
fn a_void_revalidation_withdraws_the_accept() {
    let d = knob_accepted("voidrv");
    let v = json(&prova(&d, &["exp", "revalidate", "e", "--json"]));
    assert_eq!(v["comparison"]["status"], "failed", "{v}");
    assert_eq!(
        v["best"], "baseline",
        "a void revalidation must withdraw the accept: {v}"
    );
    assert!(v.to_string().contains("withdrawn"), "{v}");
}

/// Noise must never withdraw a real accept: a machine that got 3x slower
/// for both arms moves both medians alike (the candidate/baseline ratio is
/// the same), so the revalidation is inconclusive and the accept stands.
#[test]
fn a_slower_machine_for_both_arms_keeps_the_accept() {
    let d = knob_accepted("slowrv");
    let v = json(&prova_env(
        &d,
        &["exp", "revalidate", "e", "--json"],
        &[("KNOB", "1"), ("SLOW", "1")],
    ));
    assert_eq!(v["best"], "c1", "noise must not withdraw the accept: {v}");
    assert!(v.to_string().contains("inconclusive"), "{v}");
}

/// Only the candidate's own offence withdraws its accept: a baseline trial
/// that rewrites tracked files leaves the revalidation inconclusive.
#[test]
fn a_baseline_that_tampers_in_a_revalidation_keeps_the_accept() {
    let d = knob_accepted("tamperbase");
    let v = json(&prova_env(
        &d,
        &["exp", "revalidate", "e", "--json"],
        &[("KNOB", "1"), ("REWRITE_BASE", "1")],
    ));
    assert_eq!(v["best"], "c1", "{v}");
    assert!(v.to_string().contains("inconclusive"), "{v}");
}

/// A workload that rewrites tracked files during the revalidation is the
/// candidate's own misbehaviour: the accept is withdrawn.
#[test]
fn tampering_in_a_revalidation_withdraws_the_accept() {
    let d = knob_accepted("tamperrv");
    let v = json(&prova_env(
        &d,
        &["exp", "revalidate", "e", "--json"],
        &[("KNOB", "1"), ("REWRITE_CAND", "1")],
    ));
    assert_eq!(v["best"], "baseline", "{v}");
    assert!(v.to_string().contains("changed tracked files"), "{v}");
}

/// Why medium: the scratch space is real directories that a real worker
/// process creates under the temporary directory and must remove (a
/// comparison once filled a 6 GB runtime directory with arm caches, failing
/// Go builds with "no space left on device").
#[test]
fn a_comparison_reclaims_its_scratch_space() {
    let d = repo("scratch", "0.01");
    let tmp = d.with_extension("tmpdir");
    let run = d.with_extension("rundir");
    for x in [&tmp, &run] {
        let _ = std::fs::remove_dir_all(x);
        std::fs::create_dir_all(x).unwrap();
    }
    // A worker killed outright cannot clean up: the next worker does. One
    // still alive (its lock held, as by a worker in another container
    // sharing this /tmp) is left alone.
    let scratch = |name: &str| {
        let r = tmp.join(name);
        std::fs::create_dir_all(r.join("arm-0")).unwrap();
        std::fs::write(r.join("arm-0").join("cache"), "x").unwrap();
        std::fs::File::create(r.join(".lock")).unwrap()
    };
    drop(scratch("prova-scratch-abandoned"));
    let live = scratch("prova-scratch-live");
    live.lock().unwrap();
    new_exp(&d, &[]);
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let o = Command::new(PROVA)
        .args(["exp", "run", "e", "--json"])
        .current_dir(&d)
        .env("PROVA_HOME", home_of(&d))
        .env("TMPDIR", &tmp)
        .env("XDG_RUNTIME_DIR", &run)
        .output()
        .unwrap();
    assert!(
        json(&o)["comparison"]["verdict"].is_string(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(tmp.join("prova-scratch-live/arm-0/cache").exists());
    std::fs::remove_dir_all(tmp.join("prova-scratch-live")).unwrap();
    drop(live);
    for x in [&tmp, &run] {
        let kept: Vec<String> = std::fs::read_dir(x)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("prova-"))
            .collect();
        assert!(kept.is_empty(), "left in {}: {kept:?}", x.display());
    }
}
