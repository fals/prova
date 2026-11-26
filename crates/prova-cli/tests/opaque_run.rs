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
#[cfg(target_os = "linux")]
struct CancelOnDrop<'a>(&'a Path, String);

#[cfg(target_os = "linux")]
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
// Linux only: macOS's `ps` gives no CPU time of exited children, so a loop
// of millisecond commands can look idle there (getting started, platform
// notes).
#[cfg(target_os = "linux")]
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

/// A busy child in a process group of its own (nextest starts every test
/// so) is the run's activity: the run is not stalled.
#[cfg(target_os = "linux")]
#[test]
fn a_busy_child_in_its_own_group_is_not_a_stall() {
    let d = workspace("own-group");
    let id = detach(
        &d,
        &["--stall-after", "1s"],
        // POSIX sh (dash has no $SECONDS); `setsid -w` waits if it forks.
        "setsid -w sh -c 'end=$(($(date +%s) + 5)); while [ $(date +%s) -lt $end ]; do :; done' & wait",
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
            "3s",
            "--json",
        ],
    );
    assert_eq!(w.status.code(), Some(5), "never stalled: {}", stdout(&w));
}

/// A person watching a plain `prova run -- cmd` saw nothing for 50 s: on a
/// terminal a live line now shows the run moving. In a log (agents, CI) it
/// never appears.
#[test]
fn a_live_line_on_a_terminal_and_never_in_a_log() {
    let d = workspace("live");
    let script = "i=0; while [ $i -lt 400000 ]; do i=$((i+1)); done; echo compiling; i=0; while [ $i -lt 400000 ]; do i=$((i+1)); done";
    let o = prova(&d, &["run", "--", "sh", "-c", script]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!err.contains("\x1b[2K"), "live line in a log: {err:?}");
    // Under a pseudo-terminal (util-linux `script`), the line is drawn.
    if cfg!(target_os = "linux") && Command::new("script").arg("--version").output().is_ok() {
        let log = d.join("tty.log");
        let cmd = format!("{PROVA} run -- sh -c '{script}'");
        Command::new("script")
            .args(["-q", "-e", "-c", &cmd])
            .arg(&log)
            .current_dir(&d)
            .env("PROVA_HOME", home_of(&d))
            .output()
            .unwrap();
        let tty = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            tty.contains("\r\x1b[2K"),
            "no live line on a terminal: {tty:?}"
        );
    }
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
    // A usage mistake: exit 2 (round 12; it was 4, "infrastructure").
    let none = prova(&d, &["run", "nope"]);
    assert_eq!(none.status.code(), Some(2));
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
    assert_eq!(leading_dash.status.code(), Some(2));
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
    assert_eq!(o.status.code(), Some(2), "a usage error (ADR 0038 §5)");
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

/// The processes carrying the run's id in their environment: the command
/// and what it started, never the supervisor.
#[cfg(target_os = "linux")]
fn run_processes(run: &str, not: u64) -> Vec<u64> {
    let want = format!("PROVA_RUN_ID={run}");
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u64>().ok())
        .filter(|pid| *pid != not)
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|env| env.split(|b| *b == 0).any(|v| v == want.as_bytes()))
        })
        .collect()
}

/// Round 9: when the supervisor died, the command ran on and `cancel` said
/// only "already aborted". #125: the supervisor is killed the moment the
/// command exists (before it could have flushed anything), so nothing but
/// the run's own start record can find the command again. Bounded waits;
/// the run's processes are found by the run id they carry.
#[cfg(target_os = "linux")]
#[test]
fn a_dead_supervisor_does_not_leave_the_command_running() {
    let d = workspace("deadsup");
    let id = detach(&d, &[], "exec sleep 31.4159");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let sup = loop {
        let runs: serde_json::Value =
            serde_json::from_str(&stdout(&prova(&d, &["status", "--json"]))).unwrap();
        let pid = runs
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .and_then(|r| r["supervisor_pid"].as_u64());
        match pid {
            Some(pid) => break pid,
            None if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            None => panic!("the run never recorded its supervisor"),
        }
    };
    while run_processes(&id, sup).is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        !run_processes(&id, sup).is_empty(),
        "the command never started"
    );
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
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    prova(&d, &["status"]); // reaps the orphan
    // The command dies of the signal the reap sends: wait (bounded) for it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut left = run_processes(&id, sup);
    while !left.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        left = run_processes(&id, sup);
    }
    assert!(left.is_empty(), "left running: {left:?}");
}

/// Round 9: `--select` with a command said "give either a suite name or a
/// command, not both" though no suite was given.
#[test]
fn select_with_a_command_says_what_it_needs() {
    let d = workspace("selcmd");
    let o = prova(&d, &["run", "--select", "x", "--", "true"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("prova adopt --write"));
}

/// Usability round 12 (a 53-test Go suite with 670 subtests): output that
/// had already arrived when the command exited was dropped once recording
/// it took longer than the 1 s drain, which history makes likely. Medium:
/// needs prova's store and a real command.
#[test]
fn every_line_that_arrived_is_recorded_however_long_recording_takes() {
    let d = workspace("drain-all");
    let script = "i=0; while [ $i -lt 400 ]; do echo \"=== RUN   TestT$i\"; echo \"--- PASS: TestT$i (0.00s)\"; i=$((i+1)); done";
    for _ in 0..2 {
        let o = prova(
            &d,
            &["run", "--json", "--until", "done", "--", "sh", "-c", script],
        );
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v["counts"]["passed"], 400, "{v}");
    }
}

/// Usability round 12: a Go test with subtests was counted once per subtest
/// plus once itself (a 53-test suite reported 723). Subtests are counted
/// apart, in Go's text and JSON forms alike. Medium: prova's store.
#[test]
fn go_subtests_are_counted_apart_from_tests() {
    let d = workspace("go-subtests");
    let text = "printf '=== RUN   TestA\\n=== RUN   TestA/one\\n=== RUN   TestA/one/deep\\n=== RUN   TestA/two\\n    --- PASS: TestA/one/deep (0.00s)\\n    --- PASS: TestA/one (0.00s)\\n    --- FAIL: TestA/two (0.00s)\\n--- FAIL: TestA (0.00s)\\n=== RUN   TestB\\n--- PASS: TestB (0.00s)\\nFAIL\\n'; exit 1";
    let json = "for e in 'run\",\"Test\":\"TestA' 'run\",\"Test\":\"TestA/one' 'pass\",\"Test\":\"TestA/one' 'fail\",\"Test\":\"TestA' 'run\",\"Test\":\"TestB' 'pass\",\"Test\":\"TestB'; do echo \"{\\\"Package\\\":\\\"p\\\",\\\"Action\\\":\\\"$e\\\"}\"; done; exit 1";
    for (script, subtests, failed) in [(text, 3, 1), (json, 1, 0)] {
        let o = prova(
            &d,
            &["run", "--json", "--until", "done", "--", "sh", "-c", script],
        );
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        let c = &v["counts"];
        assert_eq!(
            (c["passed"].as_u64(), c["failed"].as_u64()),
            (Some(1), Some(1)),
            "{v}"
        );
        assert_eq!(c["subtests"], subtests, "{v}");
        assert_eq!(c["subtests_failed"].as_u64().unwrap_or(0), failed, "{v}");
    }
    let o = prova(&d, &["run", "--until", "done", "--", "sh", "-c", text]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("tests: 1 passed, 1 failed, 0 errored, 0 skipped (and 3 subtests, 1 failed)"),
        "{out}"
    );
}

/// The command's output excerpt comes first and prova's verdict last: what
/// is read last is what prova concluded (owner, round 12). Medium: a real
/// command.
#[test]
fn the_verdict_comes_after_the_output_excerpt() {
    let d = workspace("verdict-last");
    let o = prova(
        &d,
        &[
            "run",
            "--until",
            "done",
            "--",
            "sh",
            "-c",
            "echo 'error: linker failed'; exit 2",
        ],
    );
    let out = String::from_utf8_lossy(&o.stdout);
    let excerpt = out.find("output (last").expect(&out);
    let header = out.find("\nrun ").expect(&out);
    assert!(excerpt < header, "{out}");
    assert!(
        out.trim_end().lines().last().unwrap().starts_with("next: "),
        "{out}"
    );
}

/// Round 18: `prova status` printed the output excerpt before the line that
/// says which run it is. Medium: a real run, then `status`.
#[test]
fn status_names_the_run_before_its_output_excerpt() {
    let d = workspace("status-header");
    let o = prova(
        &d,
        &[
            "run",
            "--until",
            "done",
            "--",
            "sh",
            "-c",
            "echo 'error: linker failed'; exit 2",
        ],
    );
    let id = stdout(&o)
        .lines()
        .find_map(|l| l.strip_prefix("run "))
        .and_then(|l| l.split_whitespace().next())
        .unwrap()
        .to_owned();
    let out = stdout(&prova(&d, &["status", &id]));
    let excerpt = out.find("output (last").expect(&out);
    let header = out.find("run ").expect(&out);
    assert!(header < excerpt, "{out}");
}

/// Round 12 count audit: a name-based subtest rule took JUnit's `x/y`
/// (a display or parameterized name) for a subtest of `x` and hid its
/// failure among "subtests". Only recognizers flag subtests. Medium: a
/// result file found after the run.
#[test]
fn a_slash_in_a_junit_name_is_not_a_subtest() {
    let d = workspace("junit-slash");
    let o = prova(
        &d,
        &[
            "run",
            "--json",
            "--until",
            "done",
            "--",
            "sh",
            "-c",
            "i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; printf '<testsuite name=\"C\"><testcase classname=\"C\" name=\"x\"/><testcase classname=\"C\" name=\"x/y\"><failure message=\"boom\"/></testcase></testsuite>' > junit.xml; exit 1",
        ],
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let c = &v["counts"];
    assert_eq!(
        (c["passed"].as_u64(), c["failed"].as_u64()),
        (Some(1), Some(1)),
        "{v}"
    );
    assert!(c["subtests"].is_null(), "{v}");
}

/// Round 12 count audit: VSTest's console names NUnit/MSTest tests by bare
/// method, the TRX by class and method; each live result now covers one
/// file result, so three classes' `Same` are three tests, not one (merged)
/// or five (counted twice). Medium: a TRX found after the run.
#[test]
fn console_and_trx_results_are_each_test_once() {
    let d = workspace("trx-merge");
    let trx = r#"<TestRun xmlns="http://microsoft.com/schemas/VisualStudio/TeamTest/2010"><TestDefinitions><UnitTest name="Same" id="1"><TestMethod className="Nu.A" name="Same"/></UnitTest><UnitTest name="Same" id="2"><TestMethod className="Nu.B" name="Same"/></UnitTest><UnitTest name="Same" id="3"><TestMethod className="Nu.C" name="Same"/></UnitTest><UnitTest name="Fails" id="4"><TestMethod className="Nu.A" name="Fails"/></UnitTest></TestDefinitions><Results><UnitTestResult testId="1" testName="Same" outcome="Passed"/><UnitTestResult testId="2" testName="Same" outcome="Passed"/><UnitTestResult testId="3" testName="Same" outcome="Failed"/><UnitTestResult testId="4" testName="Fails" outcome="Failed"/></Results></TestRun>"#;
    std::fs::write(d.join("r.trx.src"), trx).unwrap();
    let script = "i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; printf '  Passed Same [1 ms]\\n  Passed Same [1 ms]\\n  Failed Same [1 ms]\\n  Failed Fails [1 ms]\\n'; mkdir -p TestResults; cp r.trx.src TestResults/r.trx; exit 1";
    let o = prova(
        &d,
        &["run", "--json", "--until", "done", "--", "sh", "-c", script],
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let c = &v["counts"];
    assert_eq!(
        (c["passed"].as_u64(), c["failed"].as_u64()),
        (Some(2), Some(2)),
        "{v}"
    );
    // Round 14: the level named only the console though the TRX counted.
    assert_eq!(v["level"], "recognized", "{v}");
    assert_eq!(v["with_result_files"], true, "{v}");
}

/// Round 13 (Java, C#): a result file the previous run wrote moments before
/// counted for the next run (a 2 s mtime slack): a test that never ran was
/// recorded, a selection that matched nothing passed, a real failure was
/// overridden by an old pass. Only files this run created or changed count.
/// Medium: result files on disk across two runs.
#[test]
fn a_previous_runs_result_file_is_not_this_runs() {
    let d = workspace("stale-results");
    let busy = "i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done";
    let first = format!(
        "{busy}; printf '<testsuite name=\"C\"><testcase classname=\"C\" name=\"old\"><failure message=\"boom\"/></testcase></testsuite>' > junit.xml; exit 1"
    );
    prova(
        &d,
        &["run", "--json", "--until", "done", "--", "sh", "-c", &first],
    );
    let second = format!("{busy}; echo 'test m::new ... ok'; exit 0");
    let o = prova(
        &d,
        &[
            "run", "--json", "--until", "done", "--", "sh", "-c", &second,
        ],
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let c = &v["counts"];
    assert_eq!(
        (c["passed"].as_u64(), c["failed"].as_u64()),
        (Some(1), Some(0)),
        "{v}"
    );
    // The same file rewritten by this run does count.
    let third = format!(
        "{busy}; printf '<testsuite name=\"C\"><testcase classname=\"C\" name=\"old\"/></testsuite>' > junit.xml; exit 0"
    );
    let o = prova(
        &d,
        &["run", "--json", "--until", "done", "--", "sh", "-c", &third],
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["counts"]["passed"], 1, "{v}");
}

/// Round 13 (Go, red team): subtests were never written to history (a
/// pattern matched only non-subtests), so a failing subtest had no label
/// and `prova history` did not know it. Medium: the store across runs.
#[test]
fn a_subtest_has_history() {
    let d = workspace("subtest-history");
    let script = "i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; printf '=== RUN   TestA\\n=== RUN   TestA/two\\n    --- FAIL: TestA/two (0.00s)\\n--- FAIL: TestA (0.00s)\\nFAIL\\n'; exit 1";
    prova(&d, &["run", "--until", "done", "--", "sh", "-c", script]);
    let o = prova(&d, &["run", "--until", "done", "--", "sh", "-c", script]);
    let out = stdout(&o);
    assert!(out.contains("FAIL TestA/two [failed 1x before]"), "{out}");
    let h = stdout(&prova(&d, &["history", "TestA/two"]));
    assert!(h.contains("2 runs with a result"), "{h}");
}

/// Round 13 (Java, C#): tests known only from result files at the end had
/// no sampled test phase, so every size stayed "unknown" after 25 runs.
/// The whole run is their phase. Medium: real processes, sampled over time.
// Linux only: macOS samples through `ps` and `lsof`, which on CI runners
// can take longer than this 3 s run allows for two samples (main 3c8473a);
// tests/sizes.rs covers macOS with longer runs. bash's `$SECONDS` keeps the
// run busy without forking: `$(date +%s)` forked a shell the sampler could
// catch, making the test "medium" (main CI eb24731, failed twice).
#[cfg(target_os = "linux")]
#[test]
fn tests_known_only_from_result_files_get_a_size() {
    let d = workspace("file-sizes");
    let script = |extra: &str| {
        format!(
            "echo building; SECONDS=0; while [ $SECONDS -lt 4 ]; do :; done; {extra} printf '<testsuite name=\"C\"><testcase classname=\"C\" name=\"a\"/><testcase classname=\"C\" name=\"b\"/></testsuite>' > junit.xml"
        )
    };
    prova(
        &d,
        &["run", "--until", "done", "--", "bash", "-c", &script("")],
    );
    let h = stdout(&prova(&d, &["history", "C::a"]));
    assert!(h.contains("size: small?"), "{h}");
    let d2 = workspace("file-sizes-child");
    prova(
        &d2,
        &[
            "run",
            "--until",
            "done",
            "--",
            "bash",
            "-c",
            &script("sleep 1.2;"),
        ],
    );
    let h = stdout(&prova(&d2, &["history", "C::a"]));
    assert!(
        h.contains("size: medium or less") && h.contains("sleep"),
        "{h}"
    );
}

/// Round 13 (C#): a cancelled run was listed as "cancelled errored", read
/// as a test error. Medium: a real run cancelled.
#[test]
fn a_cancelled_run_has_no_test_outcome_in_the_list() {
    let d = workspace("cancel-list");
    let o = prova(
        &d,
        &[
            "run",
            "--detach",
            "--json",
            "--",
            "sh",
            "-c",
            "exec sleep 30",
        ],
    );
    let id = serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["run"]
        .as_str()
        .unwrap()
        .to_owned();
    prova(&d, &["cancel", &id]);
    prova(&d, &["wait", &id, "--until", "done", "--max-wait", "20s"]);
    let list = stdout(&prova(&d, &["status"]));
    let line = list
        .lines()
        .find(|l| l.starts_with(&id[..8]))
        .unwrap_or_default();
    assert!(
        line.contains("cancelled") && !line.contains("errored"),
        "{list}"
    );
}

/// Round 14: a wait that ended on its budget named the hung test without
/// saying how long it had been running; the stall default for a suite of
/// no declared size (60 s) outlasted two 30 s waits.
#[test]
fn a_wait_says_how_long_each_running_test_has_run() {
    let d = workspace("runningfor");
    let run = detach(&d, &[], "printf '=== RUN   TestHang\\n'; sleep 20");
    let o = prova(&d, &["wait", &run, "--max-wait", "2s"]);
    let out = stdout(&o);
    assert!(out.contains("running now: TestHang (for "), "{out}");
    let j = json(&prova(&d, &["status", &run, "--json"]));
    assert!(j["running_for_ms"][0].as_u64().unwrap() >= 1000, "{j}");
    prova(&d, &["cancel", &run]);
    assert_eq!(prova_core::config::DEFAULT_STALL_AFTER.as_secs(), 20);
}

/// Round 14: a stalled run said "no activity for 5.1s" half a minute after
/// the stall, the idle time at the moment it was declared.
#[test]
fn a_stall_reports_the_idle_time_so_far() {
    let d = workspace("idle-now");
    let run = detach(&d, &["--stall-after", "1s"], "sleep 20");
    let w = json(&prova(
        &d,
        &[
            "wait",
            &run,
            "--until",
            "stall",
            "--max-wait",
            "10s",
            "--json",
        ],
    ));
    assert_eq!(w["reason"], "stall", "{w}");
    let seq = w["last_seq"].as_u64().unwrap().to_string();
    // Nothing else happens: this wait ends on its budget.
    prova(&d, &["wait", &run, "--after", &seq, "--max-wait", "2s"]);
    let s = json(&prova(&d, &["status", &run, "--json"]));
    assert!(s["stalled_ms"].as_u64().unwrap() >= 3000, "{s}");
    prova(&d, &["cancel", &run]);
}

/// Round 14: `run_finished` was stamped with the process's exit time,
/// before the size and totals events recorded ahead of it.
#[test]
fn event_timestamps_never_go_back() {
    let d = workspace("ts-order");
    prova(
        &d,
        &[
            "run",
            "--",
            "sh",
            "-c",
            "i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; echo 'test m::a ... ok'; echo 'test result: ok. 1 passed; 0 failed'",
        ],
    );
    let ev = stdout(&prova(&d, &["events", "last"]));
    let ts: Vec<u64> = ev
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["ts_ms"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert!(ts.windows(2).all(|w| w[0] <= w[1]), "{ev}");
}

/// Round 14: gotestsum's default output gave two notes saying the same
/// thing (its own, and the generic quiet-output one).
#[test]
fn gotestsum_quiet_output_is_explained_once() {
    let d = workspace("gts-once");
    let script = "i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; printf '%s\\n' '✓  example.com/m (1ms)' '✖  example.com/m/b (2ms)' '' '=== Failed' '=== FAIL: example.com/m/b TestB (0.00s)' '    b_test.go:9: boom' '' 'DONE 5 tests, 1 failure in 0.1s'; exit 1";
    let o = prova(&d, &["run", "--json", "--", "sh", "-c", script]);
    let v = json(&o);
    let notes = v["notes"].to_string();
    assert_eq!(
        notes.matches("passing tests are not recorded").count(),
        1,
        "{notes}"
    );
}

/// Round 14: tests run through `docker run` were called small ("no other
/// process or socket seen") though prova could see only the client.
/// Linux: sizes come from sampling /proc.
#[cfg(target_os = "linux")]
#[test]
fn tests_run_through_a_container_client_get_no_size() {
    let d = workspace("container");
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("docker");
    std::fs::write(
        &fake,
        "#!/bin/sh\ni=0; while [ $i -lt 2000 ]; do i=$((i+1)); done\necho 'test m::a ... ok'\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let o = Command::new(PROVA)
        .args([
            "run", "--json", "--", "docker", "run", "--rm", "img", "cargo", "test",
        ])
        .current_dir(&d)
        .env("PROVA_HOME", home_of(&d))
        .env("PATH", path)
        .output()
        .unwrap();
    let v = json(&o);
    assert!(
        v["notes"].to_string().contains("ran through `docker`"),
        "{v}"
    );
    // Its CPU is spent in the container: no "used no CPU" warning.
    assert_eq!(v["verified"], true, "{v}");
    let ev = stdout(&prova(&d, &["events", "last"]));
    assert!(
        !ev.contains("\"run_size\"") && !ev.contains("\"test_size\""),
        "{ev}"
    );
}

/// Round 14: a skipped Go subtest was counted as a subtest like one that
/// ran.
#[test]
fn skipped_subtests_are_counted_as_skipped() {
    let d = workspace("go-subtest-skip");
    let script = "i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; printf '%s\\n' '=== RUN   TestP' '=== RUN   TestP/a' '=== RUN   TestP/b' '    p_test.go:9: not here' '--- PASS: TestP (0.00s)' '    --- PASS: TestP/a (0.00s)' '    --- SKIP: TestP/b (0.00s)' 'PASS' 'ok  \texample.com/m\t0.01s'";
    let o = prova(&d, &["run", "--", "sh", "-c", script]);
    let out = stdout(&o);
    assert!(
        out.contains("(and 2 subtests, 0 failed, 1 skipped)"),
        "{out}"
    );
}

/// Round 15: selecting one case of a parameterized test ran all of them,
/// with no word of it (Surefire selects methods).
#[test]
fn selecting_one_parameterized_case_says_every_case_runs() {
    let d = workspace("param-select");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.j]\nrun = \"echo all\"\nselect = \"echo mvn -q test -Dtest={filter}; i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; echo 'test calc::x ... ok'\"\n",
    )
    .unwrap();
    let o = prova(
        &d,
        &[
            "run",
            "j",
            "--json",
            "--select",
            "calc.CalcTest::many(int, int)[2]",
        ],
    );
    let v = json(&o);
    assert!(
        v["notes"]
            .to_string()
            .contains("not one of its cases: every case of it runs"),
        "{v}"
    );
}

/// A fake `docker` on PATH: `run` prints a test line and hangs; `kill`
/// records its arguments in `killed`.
#[cfg(target_os = "linux")]
fn fake_docker(d: &Path) -> (String, PathBuf) {
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let killed = d.join("killed");
    let fake = bin.join("docker");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\ncase \"$1\" in kill) echo \"$@\" > {k}; exit 0;; esac\necho 'test m::a ... ok'\nexec sleep 60\n",
            k = killed.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    (path, killed)
}

#[cfg(target_os = "linux")]
fn cancel_fake_docker(d: &Path, path: &str, args: &[&str]) -> serde_json::Value {
    let mut a = vec!["run", "--detach", "--json", "--", "docker", "run", "--rm"];
    a.extend_from_slice(args);
    let o = Command::new(PROVA)
        .args(a)
        .current_dir(d)
        .env("PROVA_HOME", home_of(d))
        .env("PATH", path)
        .output()
        .unwrap();
    let id = json(&o)["run"].as_str().unwrap().to_owned();
    let _guard = CancelOnDrop(d, id.clone());
    // The command must be running (its container client started) before
    // the cancel: under load a cancel came "before start" (round 16 CI).
    // Bounded: each wait returns within 500 ms.
    for _ in 0..40 {
        let w = json(&prova(d, &["wait", &id, "--max-wait", "500ms", "--json"]));
        if w["status"] == "running" && w["counts"]["passed"].as_u64().unwrap_or(0) > 0 {
            break;
        }
    }
    assert!(prova(d, &["cancel", &id]).status.success());
    json(&prova(
        d,
        &[
            "wait",
            &id,
            "--until",
            "done",
            "--max-wait",
            "20s",
            "--json",
        ],
    ))
}

/// Round 15: cancelling a run through a container client left the
/// container running. A named container is attributable to the run: prova
/// kills it, and says so.
#[cfg(target_os = "linux")]
#[test]
fn cancel_stops_a_named_container_of_the_run() {
    let d = workspace("cancel-container");
    let (path, killed) = fake_docker(&d);
    let s = cancel_fake_docker(&d, &path, &["--name", "pv-x", "img", "t"]);
    assert_eq!(s["status"], "cancelled", "{s}");
    assert_eq!(
        std::fs::read_to_string(&killed).unwrap_or_default().trim(),
        "kill pv-x",
        "{s}"
    );
    assert!(s.to_string().contains("pv-x"), "{s}");
}

/// ADR 0037: a run killed at `kill_after` stops the container its client
/// named too, written `--name=x`; a `--name` in the container's own command
/// names nothing. Why medium: a real supervisor and a fake client.
#[cfg(target_os = "linux")]
#[test]
fn kill_after_stops_the_named_container_and_only_that() {
    let d = workspace("killafter-container");
    let (path, killed) = fake_docker(&d);
    let run = |args: &[&str]| {
        let _ = std::fs::remove_file(&killed);
        let mut a = vec![
            "run",
            "--kill-after",
            "2s",
            "--json",
            "--",
            "docker",
            "run",
            "--rm",
        ];
        a.extend_from_slice(args);
        let o = Command::new(PROVA)
            .args(a)
            .current_dir(&d)
            .env("PROVA_HOME", home_of(&d))
            .env("PATH", &path)
            .output()
            .unwrap();
        (
            json(&o),
            std::fs::read_to_string(&killed).unwrap_or_default(),
        )
    };
    let (s, k) = run(&["--name=pv-y", "img", "t"]);
    assert_eq!(k.trim(), "kill pv-y", "{s}");
    let (s, k) = run(&["img", "t", "--name", "not-a-container"]);
    assert!(k.is_empty(), "killed {k:?} the command never named: {s}");
}

/// Without a name or cidfile the container is not known: prova does not
/// guess, it says it may still run and how to stop it.
#[cfg(target_os = "linux")]
#[test]
fn cancel_warns_when_the_container_cannot_be_attributed() {
    let d = workspace("cancel-container-anon");
    let (path, killed) = fake_docker(&d);
    let s = cancel_fake_docker(&d, &path, &["img", "t"]);
    assert_eq!(s["status"], "cancelled", "{s}");
    assert!(!killed.exists(), "{s}");
    let text = s.to_string();
    assert!(
        text.contains("does not stop its container")
            && text.contains("may still run")
            && text.contains("docker ps")
            && text.contains("--name"),
        "{text}"
    );
}

/// Round 18: `--until first-failure` through a container client announced
/// 30 s and waited 65. The announcement now says 1m05s. Medium: a real
/// `prova run` process and the fake client.
#[cfg(target_os = "linux")]
#[test]
fn a_container_first_wait_announces_its_real_length() {
    use std::io::BufRead;
    let d = workspace("container-announce");
    let (path, _killed) = fake_docker(&d);
    let mut child = Command::new(PROVA)
        .args([
            "run",
            "--until",
            "first-failure",
            "--",
            "docker",
            "run",
            "--rm",
            "--name",
            "pv-announce",
            "img",
            "t",
        ])
        .current_dir(&d)
        .env("PROVA_HOME", home_of(&d))
        .env("PATH", &path)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let id = line.split_whitespace().nth(1).unwrap_or("").to_owned();
    let _ = child.kill();
    let _ = child.wait();
    let _guard = CancelOnDrop(&d, id.clone());
    // Running (the client started) before the guard cancels: bounded waits.
    for _ in 0..40 {
        let w = json(&prova(&d, &["wait", &id, "--max-wait", "500ms", "--json"]));
        if w["status"] == "running" && w["counts"]["passed"].as_u64().unwrap_or(0) > 0 {
            break;
        }
    }
    assert!(
        line.contains("waiting up to 1m05s for the first failure"),
        "{line}"
    );
}

/// Round 18 N8: `docker exec` runs a process in a container that is not the
/// run's; the warning must not tell to stop that container.
#[cfg(target_os = "linux")]
#[test]
fn cancel_through_docker_exec_does_not_blame_the_container() {
    let d = workspace("cancel-container-exec");
    let (path, killed) = fake_docker(&d);
    let o = Command::new(PROVA)
        .args([
            "run", "--detach", "--json", "--", "docker", "exec", "dev", "t",
        ])
        .current_dir(&d)
        .env("PROVA_HOME", home_of(&d))
        .env("PATH", &path)
        .output()
        .unwrap();
    let id = json(&o)["run"].as_str().unwrap().to_owned();
    let _guard = CancelOnDrop(&d, id.clone());
    for _ in 0..40 {
        let w = json(&prova(&d, &["wait", &id, "--max-wait", "500ms", "--json"]));
        if w["status"] == "running" && w["counts"]["passed"].as_u64().unwrap_or(0) > 0 {
            break;
        }
    }
    assert!(prova(&d, &["cancel", &id]).status.success());
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
    assert!(!killed.exists(), "{s}");
    let text = s.to_string();
    assert!(
        text.contains("docker exec") && text.contains("may still run inside that container"),
        "{text}"
    );
    assert!(!text.contains("check `docker ps` and stop it"), "{text}");
}

/// Round 15: a selected Vitest run listed another file's import error next
/// to the selected test, with no word that it was outside the selection.
#[test]
fn a_failure_outside_the_selection_says_so() {
    let d = workspace("outside-sel");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.v]\nrun = \"echo all\"\nselect = \"i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; printf '%s\\\\n' ' \u{2713} src/group.test.ts > group > bad two 1ms' ' FAIL  src/broken.test.ts [ src/broken.test.ts ]' 'Error: Cannot find module ./missing' ' Test Files  1 failed | 1 passed (2)' '      Tests  1 passed (1)'; exit 1\"\n",
    )
    .unwrap();
    let o = prova(&d, &["run", "v", "--select", "group > bad two"]);
    let out = stdout(&o);
    assert!(
        out.contains("outside the selection \"group > bad two\": src/broken.test.ts::(file)"),
        "{out}"
    );
}

/// Round 16: a cancelled run listed its hung test among the slow tests,
/// with the time the cancel let it run.
#[test]
fn a_cancelled_hang_is_not_a_slow_test() {
    let d = workspace("cancel-slow");
    let run = detach(&d, &[], "printf '=== RUN   TestHang\\n'; sleep 20");
    prova(&d, &["wait", &run, "--max-wait", "1500ms"]);
    let o = prova(&d, &["cancel", &run]);
    let out = stdout(&o);
    assert!(
        out.contains("CANCELLED") && !out.contains("slow tests:"),
        "{out}"
    );
}

/// Round 16: a NuGet restore inside docker was called a stall at 20 s: the
/// host process tree is idle by design when the tests run in a container.
/// Before a minute, process inactivity alone is no stall there.
#[cfg(target_os = "linux")]
#[test]
fn a_container_run_is_not_stalled_by_an_idle_process_tree() {
    let d = workspace("container-stall");
    let (path, _killed) = fake_docker(&d);
    let o = Command::new(PROVA)
        .args([
            "run",
            "--detach",
            "--json",
            "--stall-after",
            "1s",
            "--",
            "docker",
            "run",
            "--rm",
            "--name",
            "pv-idle",
            "img",
            "t",
        ])
        .current_dir(&d)
        .env("PROVA_HOME", home_of(&d))
        .env("PATH", &path)
        .output()
        .unwrap();
    let id = json(&o)["run"].as_str().unwrap().to_owned();
    let _guard = CancelOnDrop(&d, id.clone());
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
        "not stalled before a minute: {}",
        stdout(&w)
    );
}

/// Issue #89: CPU used a while ago is not recent activity. A test process
/// busy for 2 s (collection, imports) then hung in `sleep` was credited as
/// active for the whole 5 s busy window after its last tick, so
/// `--stall-after 2s` fired at about 9 s, not 4 s. Medium: needs a real
/// process tree with real CPU use.
#[test]
fn a_stall_is_counted_from_the_last_cpu_use_not_the_busy_window() {
    let d = workspace("stallclock");
    let id = detach(
        &d,
        &["--stall-after", "2s"],
        "perl -e '$t = time; while (time - $t < 2) {}'; echo done collecting; exec sleep 60",
    );
    let w = prova(
        &d,
        &[
            "wait",
            &id,
            "--until",
            "stall",
            "--max-wait",
            "20s",
            "--json",
        ],
    );
    assert_eq!(w.status.code(), Some(3), "stalled: {}", stdout(&w));
    let s = json(&w);
    let elapsed = s["elapsed_ms"].as_u64().unwrap();
    assert!(
        elapsed < 6500,
        "last CPU at ~2 s, stall-after 2 s: reported at {elapsed} ms: {s}"
    );
    prova(&d, &["cancel", &id]);
}
