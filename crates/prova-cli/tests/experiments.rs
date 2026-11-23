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
        v["comparison"]["result"]["comparisons_in_family"], 2,
        "Holm family grows"
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
    let sandboxed = r["comparison"]["result"]["sandbox"]
        .as_str()
        .is_some_and(|s| s.starts_with("bubblewrap"));
    if sandboxed {
        // ADR 0024: the process dies with the trial's sandbox; nothing runs on.
        // The namespace is torn down asynchronously: wait (bounded) for it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let left = loop {
            let o = Command::new("pgrep")
                .args(["-fx", "sleep 37.125"])
                .output()
                .unwrap();
            if o.stdout.is_empty() || std::time::Instant::now() > deadline {
                break o.stdout;
            }
            std::thread::yield_now();
        };
        assert!(left.is_empty(), "left running: {r}");
    } else {
        assert_eq!(r["comparison"]["status"], "failed", "{r}");
        let reason = r["comparison"]["result"]["reason"].as_str().unwrap();
        assert!(
            reason.contains("left process") && reason.contains("sleep"),
            "{reason}"
        );
    }
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

/// Round 10 red team: a top-level `random.py` shadowed the standard library
/// for the oracle; it is a file that decides the verdict.
#[test]
fn a_module_shadowing_the_python_standard_library_is_out_of_scope() {
    let d = repo("stdlib", "0.01");
    new_exp(&d, &[]);
    std::fs::write(d.join("random.py"), "def randrange(a, b): return a\n").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    let reason = r["comparison"]["result"]["reason"].as_str().unwrap_or("");
    assert!(
        reason.contains("out of scope") && reason.contains("random.py"),
        "{r}"
    );
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
        "#!/bin/sh\n[ \"$PYTHONNOUSERSITE\" = 1 ] && [ -n \"$PYTHONPYCACHEPREFIX\" ] || exit 3\necho '{\"metric\":\"t\",\"value\":5}'\n",
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
/// baseline's compiled test; secrets reached trials. Each arm gets its own
/// caches, and secret-named variables are removed.
#[test]
fn trials_get_private_caches_and_no_secrets() {
    let d = std::env::temp_dir().join(format!("prova-exp-caches-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("work.sh"),
        "#!/bin/sh\ncase \"$GOCACHE\" in *prova-arm-*) ;; *) exit 3;; esac\n[ -z \"$MY_API_TOKEN\" ] || exit 4\necho '{\"metric\":\"t\",\"value\":5}'\n",
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

/// Round 11 red team: `git replace` swapped the baseline's content; prova's
/// git ignores replace objects, so the baseline is the commit itself.
#[test]
fn replace_objects_do_not_change_what_runs() {
    let d = repo("replace", "0.01");
    new_exp(&d, &[]);
    // Replace the baseline's `delay` blob with a slower one.
    sh(
        &d,
        "orig=$(git rev-parse HEAD:delay); slow=$(printf 0.08 | git hash-object -w --stdin); git replace $orig $slow",
    );
    std::fs::write(d.join("delay"), "0.012").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let r = json(&prova(&d, &["exp", "run", "e", "--json"]));
    assert_ne!(
        r["comparison"]["verdict"], "accept",
        "a slowed baseline was measured: {r}"
    );
}

/// Round 11 red team: a local smudge filter rewrote the baseline at
/// checkout; the checkout is verified against the commit's own bytes.
#[test]
fn a_checkout_changed_by_a_git_filter_is_refused() {
    let d = repo("smudge", "0.01");
    new_exp(&d, &[]);
    sh(
        &d,
        "git config filter.slow.smudge 'sed s/0.01/0.08/' && git config filter.slow.clean cat && echo 'delay filter=slow' > .git/info/attributes",
    );
    std::fs::write(d.join("other"), "x").unwrap();
    assert!(prova(&d, &["exp", "candidate", "e"]).status.success());
    let o = prova(&d, &["exp", "run", "e", "--json"]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(text.contains("differs from commit"), "{text}");
}

/// Round 11 red team: editing prova.db turned a REJECT into an ACCEPT;
/// `exp status` re-derives every ACCEPT from the samples.
#[test]
fn an_edited_verdict_shows_in_status() {
    let d = repo("edited", "0.01");
    new_exp(&d, &[]);
    std::fs::write(d.join("delay"), "0.011").unwrap();
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
