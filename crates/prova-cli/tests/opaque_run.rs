//! Medium tests: real processes, real store, the real binary (ADR 0005).
//! No sleeps: every wait goes through `prova wait` with a bound.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
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

fn detach(dir: &Path, extra: &[&str], script: &str) -> String {
    let mut args = vec!["run", "--detach", "--json"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--", "sh", "-c", script]);
    let o = prova(dir, &args);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    json(&o)["run"].as_str().unwrap().to_owned()
}

#[test]
fn passing_and_failing_commands_map_to_exit_codes() {
    let d = workspace("exit");
    let ok = prova(&d, &["run", "--", "sh", "-c", "echo fine"]);
    assert_eq!(ok.status.code(), Some(0), "{}", stdout(&ok));
    let bad = prova(
        &d,
        &["run", "--json", "--", "sh", "-c", "echo 'boom 42'; exit 3"],
    );
    assert_eq!(bad.status.code(), Some(1));
    let s = json(&bad);
    assert_eq!(s["outcome"], "failed");
    assert_eq!(s["exit_code"], 3);
    assert_eq!(s["level"], "opaque");
    assert_eq!(s["output_tail"][0], "boom 42");
}

#[test]
fn same_failure_is_recognised_across_runs() {
    let d = workspace("history");
    let script = "echo 'expected 1 got 2 at 0xdead'; exit 1";
    prova(&d, &["run", "--", "sh", "-c", script]);
    let second = json(&prova(&d, &["run", "--json", "--", "sh", "-c", script]));
    assert_eq!(second["history"]["previous_runs"], 1);
    assert_eq!(second["history"]["same_failure_seen"], 1);
}

#[test]
fn idle_process_is_reported_as_stall_and_can_be_cancelled() {
    let d = workspace("stall");
    let id = detach(&d, &["--stall-after", "1s"], "echo up; exec sleep 60");
    let w = prova(&d, &["wait", &id, "--max-wait", "20s", "--json"]);
    assert_eq!(w.status.code(), Some(3), "{}", stdout(&w));
    assert_eq!(json(&w)["reason"], "stall");
    assert!(prova(&d, &["cancel", &id]).status.success());
    let done = prova(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "done",
            "--max-wait",
            "20s",
            "--json",
        ],
    );
    let s = json(&done);
    assert_eq!(s["status"], "cancelled");
    assert_eq!(done.status.code(), Some(3));
}

#[test]
fn busy_process_is_not_a_stall() {
    let d = workspace("busy");
    let id = detach(&d, &["--stall-after", "1s"], "while :; do :; done");
    let w = prova(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "stall",
            "--max-wait",
            "3s",
            "--json",
        ],
    );
    assert_eq!(
        w.status.code(),
        Some(5),
        "still running, never stalled: {}",
        stdout(&w)
    );
    prova(&d, &["cancel", &id]);
    prova(&d, &["wait", &id, "--until", "done", "--max-wait", "20s"]);
}

/// Cancels a run when dropped, so a failing assertion cannot leave an
/// endless workload behind (it froze a CI runner).
struct CancelOnDrop<'a>(&'a Path, String);

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        prova(self.0, &["cancel", &self.1]);
        prova(
            self.0,
            &["wait", &self.1, "--until", "done", "--max-wait", "20s"],
        );
    }
}

/// A silent loop of short-lived commands is busy: their CPU shows in the
/// shell's reaped-children time (the release smoke test saw it stall).
/// (It once froze the arm64 runner: not itself, but by pushing pids past
/// 10000, where another test's `kill -9 -<pgid>` became `kill -1`.)
#[test]
fn a_loop_of_short_commands_is_not_a_stall() {
    let d = workspace("short-cmds");
    let id = detach(
        &d,
        &["--stall-after", "1s"],
        "while :; do date >/dev/null; done",
    );
    let _cancel = CancelOnDrop(&d, id.clone());
    let w = prova(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "stall",
            "--max-wait",
            "4s",
            "--json",
        ],
    );
    assert_eq!(w.status.code(), Some(5), "never stalled: {}", stdout(&w));
}

#[test]
fn kill_after_bounds_the_run() {
    let d = workspace("ceiling");
    let id = detach(&d, &["--kill-after", "1s"], "exec sleep 60");
    let w = prova(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "done",
            "--max-wait",
            "20s",
            "--json",
        ],
    );
    let s = json(&w);
    assert_eq!(s["status"], "aborted");
    assert!(s["detail"].as_str().unwrap().starts_with("killed"));
    assert_eq!(w.status.code(), Some(3));
}

#[test]
fn leaked_children_are_reported_and_killed() {
    let d = workspace("leak");
    let id = detach(&d, &[], "sleep 60 & echo spawned");
    let s = json(&prova(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "done",
            "--max-wait",
            "20s",
            "--json",
        ],
    ));
    assert_eq!(s["outcome"], "passed");
    let leaked = s["leaked"].as_array().expect("leaked list");
    assert_eq!(leaked.len(), 1);
    let pid = leaked[0].as_u64().unwrap();
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists() || {
            let st = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            st.contains(") Z ")
        }
    );
}

#[test]
fn dead_supervisor_leaves_run_aborted_not_running() {
    let d = workspace("orphan");
    let id = detach(&d, &[], "exec sleep 60");
    let status = json(&prova(
        &d,
        &["wait", &id, "--until", "done", "--max-wait", "1s", "--json"],
    ));
    assert_eq!(status["status"], "running");
    let pgid = run_started_pid(&d, &id);
    Command::new("kill")
        .args(["-9", &supervisor_pid(&d, &id).to_string()])
        .status()
        .unwrap();
    let s = json(&prova(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "done",
            "--max-wait",
            "10s",
            "--json",
        ],
    ));
    assert_eq!(s["status"], "aborted");
    // The command outlived its supervisor; prova killed what carries the
    // run's id when it found the run orphaned (`cancel` would too). (A shell `kill -9 -<pgid>` was
    // misread by the arm64 runner's util-linux as `kill(-1, SIGKILL)` for
    // ids starting with 1, killing the whole runner.)
    prova(&d, &["cancel", &id]);
    assert!(
        !std::path::Path::new(&format!("/proc/{pgid}")).exists() || {
            let st = std::fs::read_to_string(format!("/proc/{pgid}/stat")).unwrap_or_default();
            st.contains(") Z ")
        }
    );
}

fn run_started_pid(d: &Path, id: &str) -> u64 {
    let o = prova(d, &["events", id]);
    let first: serde_json::Value =
        serde_json::from_str(stdout(&o).lines().next().unwrap()).unwrap();
    first["pid"].as_u64().unwrap()
}

/// Reads the supervisor pid via `prova status --json` of the run list.
fn supervisor_pid(d: &Path, id: &str) -> u32 {
    let runs = json(&prova(d, &["status", "--json"]));
    runs.as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .and_then(|r| r["supervisor_pid"].as_u64())
        .expect("supervisor pid") as u32
}

#[test]
fn events_are_json_lines_in_order() {
    let d = workspace("events");
    prova(&d, &["run", "--", "sh", "-c", "echo x"]);
    let o = prova(&d, &["events", "last"]);
    let lines: Vec<serde_json::Value> = stdout(&o)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.first().unwrap()["kind"], "run_started");
    assert_eq!(lines.last().unwrap()["kind"], "run_finished");
    let seqs: Vec<u64> = lines.iter().map(|l| l["seq"].as_u64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1));
}

#[test]
fn suite_from_prova_toml_with_selection() {
    let d = workspace("suite");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.small]\nrun = \"echo all\"\nselect = \"echo only {filter}\"\nsize = \"small\"\n",
    )
    .unwrap();
    let o = prova(&d, &["run", "small", "--select", "parser", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    let out = prova(&d, &["output", "last"]);
    assert_eq!(stdout(&out).trim(), "only parser");
    let none = prova(&d, &["run", "nope"]);
    assert_eq!(none.status.code(), Some(4));
}

#[test]
fn selection_cannot_inject_shell_commands() {
    let d = workspace("inject");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.t]\nrun = \"echo all\"\nselect = \"echo selected {filter}\"\n",
    )
    .unwrap();
    let o = prova(
        &d,
        &["run", "t", "--select", "x; touch PWNED $(touch PWNED2)"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
    assert!(!d.join("PWNED").exists() && !d.join("PWNED2").exists());
    let out = prova(&d, &["output", "last"]);
    assert_eq!(
        stdout(&out).trim(),
        "selected x; touch PWNED $(touch PWNED2)"
    );
    let leading_dash = prova(&d, &["run", "t", "--select=-rf"]);
    assert_eq!(leading_dash.status.code(), Some(4));
}

#[test]
fn store_is_private_to_the_owner() {
    use std::os::unix::fs::PermissionsExt;
    let d = workspace("perms");
    let s = json(&prova(&d, &["run", "--json", "--", "true"]));
    let store = PathBuf::from(s["store"].as_str().expect("store path"));
    assert!(
        store.starts_with(home_of(&d)),
        "central home, not the workspace: {store:?}"
    );
    assert!(!d.join(".prova").exists(), "workspace keeps nothing");
    let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(store.clone()), 0o700);
    assert_eq!(mode(store.join("prova.db")), 0o600);
    let run = s["run"].as_str().unwrap();
    assert!(
        !store.join("runs").join(run).exists(),
        "a finished run leaves no files"
    );
    // The live spool is private while the run is going.
    let live = json(&prova(
        &d,
        &[
            "run",
            "--detach",
            "--json",
            "--",
            "sh",
            "-c",
            "echo up; sleep 30",
        ],
    ));
    let live_run = live["run"].as_str().unwrap();
    let spool = store.join("runs").join(live_run).join("output.log");
    let w = prova(
        &d,
        &[
            "wait",
            live_run,
            "--until",
            "first-failure",
            "--max-wait",
            "2s",
        ],
    );
    assert_eq!(w.status.code(), Some(5), "still running");
    assert_eq!(mode(spool), 0o600);
    assert!(prova(&d, &["cancel", live_run]).status.success());
    assert_eq!(
        prova(&d, &["status", "%"]).status.code(),
        Some(4),
        "LIKE wildcard must not resolve"
    );
}

/// Isolated prova home for a test workspace, outside it (ADR 0014).
fn home_of(dir: &Path) -> PathBuf {
    PathBuf::from(format!("{}.home", dir.display()))
}

/// ADR 0016: a finished run keeps an excerpt (head, error blocks, tail) in
/// the store, not a log file; `--keep-output` keeps everything.
#[test]
fn long_output_is_distilled_into_an_excerpt() {
    let d = workspace("excerpt");
    let script = "i=1; while [ $i -le 2000 ]; do if [ $i = 1000 ]; then echo 'error: the real cause'; else echo \"line $i\"; fi; i=$((i+1)); done; exit 3";
    let s = json(&prova(&d, &["run", "--json", "--", "sh", "-c", script]));
    let run = s["run"].as_str().unwrap();
    let store = PathBuf::from(s["store"].as_str().unwrap());
    assert!(!store.join("runs").join(run).exists());
    let out = stdout(&prova(&d, &["output", run]));
    assert!(out.starts_with("line 1\n"), "{out}");
    assert!(
        out.contains("error: the real cause"),
        "error block kept: {out}"
    );
    assert!(out.contains("lines elided ...]"), "{out}");
    assert!(out.trim_end().ends_with("line 2000"), "{out}");
    let j: serde_json::Value =
        serde_json::from_str(&stdout(&prova(&d, &["output", run, "--json"]))).unwrap();
    assert_eq!(j["excerpt"]["total_lines"], 2000);
    assert!(j["elided_lines"].as_u64().unwrap() > 1500);
    assert_eq!(
        stdout(&prova(&d, &["output", run, "--tail", "1"])).trim(),
        "line 2000"
    );

    let k = json(&prova(
        &d,
        &["run", "--json", "--keep-output", "--", "sh", "-c", script],
    ));
    let full = stdout(&prova(&d, &["output", k["run"].as_str().unwrap()]));
    assert_eq!(full.lines().count(), 2000, "whole output kept on request");
}

/// Usability round 3: `r=$(prova run --detach)` must capture just the id, and
/// prova's own flags typed after `--` are pointed out.
#[test]
fn detach_prints_only_the_id_and_misplaced_flags_are_flagged() {
    let d = workspace("detach-id");
    let o = prova(&d, &["run", "--detach", "--", "true"]);
    let id = stdout(&o).trim().to_owned();
    assert!(
        !id.is_empty() && !id.contains(char::is_whitespace),
        "stdout {id:?}"
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains(&format!("next: prova wait {id}")));
    assert!(
        prova(&d, &["wait", &id, "--until", "done", "--max-wait", "10s"])
            .status
            .success()
    );
    // Round 11: refused, not run with the flag passed to the command.
    let o = prova(&d, &["run", "--", "true", "--detach"]);
    assert_eq!(o.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&o.stderr).contains("--detach comes after `--`"));
}

/// ADR 0019: "run this and give me the result" is one command; when the
/// budget ends first, the continuation is one command too, never a loop.
#[test]
fn run_until_done_is_one_command_and_continues_with_one() {
    let d = workspace("until");
    let o = prova(
        &d,
        &[
            "run",
            "--until",
            "done",
            "--",
            "sh",
            "-c",
            "echo 'test a ... ok'",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
    assert!(stdout(&o).contains("PASSED"));
    let o = prova(
        &d,
        &[
            "run",
            "--until",
            "done",
            "--max-wait",
            "1s",
            "--",
            "sh",
            "-c",
            "sleep 4; echo 'test b ... ok'",
        ],
    );
    assert_eq!(
        o.status.code(),
        Some(5),
        "budget ended first: {}",
        stdout(&o)
    );
    let next = stdout(&o)
        .lines()
        .find_map(|l| l.strip_prefix("next: "))
        .unwrap()
        .to_owned();
    assert!(next.ends_with("--until done"), "{next}");
    let args: Vec<&str> = next.split_whitespace().skip(1).collect();
    let o = prova(&d, &args);
    assert_eq!(o.status.code(), Some(0), "{}", stdout(&o));
    assert!(
        prova(&d, &["run", "--detach", "--until", "done", "--", "true"])
            .status
            .code()
            == Some(2)
    );
}

/// ADR 0019: a long wait is not silent for the person watching: failures
/// appear on stderr as they happen; the summary stays on stdout.
#[test]
fn a_long_wait_shows_failures_as_they_happen() {
    let d = workspace("progress");
    let o = prova(
        &d,
        &[
            "run",
            "--until",
            "done",
            "--",
            "sh",
            "-c",
            "echo 'test a ... FAILED'; sleep 2; echo 'test b ... ok'; exit 1",
        ],
    );
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(err.contains("waiting up to 9m"), "{err}");
    assert!(err.contains("FAIL a"), "the failure, before the end: {err}");
    assert!(stdout(&o).contains("FAILED") && !stdout(&o).contains("waiting up to"));
}

/// Round 6: a stalled run ends a wait for the result at once, with the
/// advice to cancel, instead of waiting out the budget.
#[test]
fn a_stall_ends_a_wait_for_the_result() {
    let d = workspace("stallend");
    let started = std::time::Instant::now();
    let o = prova(
        &d,
        &[
            "run",
            "--until",
            "done",
            "--stall-after",
            "1s",
            "--",
            "sh",
            "-c",
            "sleep 30",
        ],
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "returned on the stall"
    );
    assert_eq!(o.status.code(), Some(3), "{}", stdout(&o));
    assert!(
        stdout(&o).contains("STALLED") && stdout(&o).contains("prova cancel"),
        "{}",
        stdout(&o)
    );
    let id = stdout(&o)
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    prova(&d, &["cancel", &id]);
}

/// Round 7: a bare attached `prova run -- cmd` waited forever on a hang.
#[test]
fn an_attached_run_returns_on_a_stall() {
    let d = workspace("attachedstall");
    let started = std::time::Instant::now();
    let o = prova(
        &d,
        &["run", "--stall-after", "1s", "--", "sh", "-c", "sleep 30"],
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "returned on the stall"
    );
    assert_eq!(o.status.code(), Some(3), "{}", stdout(&o));
    assert!(stdout(&o).contains("prova cancel"), "{}", stdout(&o));
    let id = stdout(&o)
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    prova(&d, &["cancel", &id]);
}

/// Round 8: an idle JVM or .NET runtime still uses a little CPU (GC, timers);
/// any tick counted as activity, so a hung xUnit/JUnit test never stalled.
#[test]
fn a_process_that_barely_ticks_still_stalls() {
    let d = workspace("ticks");
    // ~1 ms of CPU every 200 ms, no new processes: a runtime idling.
    let id = detach(
        &d,
        &["--stall-after", "2s"],
        "exec perl -e 'while (1) { for (1..20000) {} select(undef, undef, undef, 0.2) }'",
    );
    let w = prova(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "stall",
            "--max-wait",
            "15s",
            "--json",
        ],
    );
    assert_eq!(w.status.code(), Some(3), "stalled: {}", stdout(&w));
    prova(&d, &["cancel", &id]);
}

/// Round 8: pytest's progress dots have no newline until a test finishes;
/// before a hang they never reached the live output, which named nothing.
#[test]
fn an_unfinished_line_is_in_the_live_output() {
    let d = workspace("partial");
    let id = detach(&d, &["--stall-after", "1s"], "printf '....'; exec sleep 30");
    let w = prova(&d, &["wait", &id, "--until", "stall", "--max-wait", "15s"]);
    assert_eq!(w.status.code(), Some(3), "{}", stdout(&w));
    let out = prova(&d, &["output", &id]);
    assert!(stdout(&out).contains("...."), "{}", stdout(&out));
    prova(&d, &["cancel", &id]);
}

/// Round 9: when the supervisor died, the command ran on and `cancel` said
/// only "already aborted".
#[test]
fn a_dead_supervisor_does_not_leave_the_command_running() {
    let d = workspace("deadsup");
    let id = detach(&d, &[], "exec sleep 31.4159");
    let runs: serde_json::Value =
        serde_json::from_str(&stdout(&prova(&d, &["status", "--json"]))).unwrap();
    let sup = runs
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .and_then(|r| r["supervisor_pid"].as_u64())
        .expect("supervisor pid");
    std::process::Command::new("kill")
        .args(["-9", &sup.to_string()])
        .status()
        .unwrap();
    // Wait (bounded) until the supervisor is gone.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::path::Path::new(&format!("/proc/{sup}")).exists()
        && std::fs::read_to_string(format!("/proc/{sup}/stat")).is_ok_and(|s| !s.contains(") Z "))
        && std::time::Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    prova(&d, &["status"]); // reaps the orphan
    let left = std::process::Command::new("pgrep")
        .args(["-f", "sleep 31.4159"])
        .output()
        .unwrap();
    assert!(
        left.stdout.is_empty(),
        "left running: {}",
        String::from_utf8_lossy(&left.stdout)
    );
}

/// Round 9: `--select` with a command said "give either a suite name or a
/// command, not both" though no suite was given.
#[test]
fn select_with_a_command_says_what_it_needs() {
    let d = workspace("selcmd");
    let o = prova(&d, &["run", "--select", "x", "--", "true"]);
    assert_eq!(o.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&o.stderr).contains("prova adopt --write"));
}
