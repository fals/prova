//! Process-group observation and control (ADR 0004, 0013 section 3).
//!
//! The process under test leads its own process group. Activity of the
//! whole group (CPU time, process set) is sampled from `/proc` on Linux
//! and from `ps` elsewhere (macOS).

use std::collections::BTreeSet;

use rustix::process::{Pid, Signal};

/// Snapshot of a process group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupSample {
    /// Sum of user and system CPU ticks of all members.
    pub cpu_ticks: u64,
    pub pids: BTreeSet<u32>,
}

#[cfg(target_os = "linux")]
pub fn sample_group(pgid: u32) -> GroupSample {
    let mut s = GroupSample::default();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return s;
    };
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        if let Some((group, ticks, zombie)) = parse_stat(&stat)
            && group == pgid
            && !zombie
        {
            s.cpu_ticks += ticks;
            s.pids.insert(pid);
        }
    }
    s
}

/// The group and every process descended from it, even in groups of their
/// own: nextest starts each test in a new process group, and those tests
/// are the run's work (their CPU is activity; what they use decides their
/// size, ADR 0025). Kills and leftovers still go by the group.
pub fn sample_tree(pgid: u32) -> GroupSample {
    tree_of(pgid, &all_processes())
}

/// (pid, group, parent, CPU ticks, zombie) of every process.
#[cfg(target_os = "linux")]
fn all_processes() -> Vec<(u32, u32, u32, u64, bool)> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let (group, ticks, zombie) = parse_stat(&stat)?;
            let ppid = stat[stat.rfind(')')? + 2..]
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()?;
            Some((pid, group, ppid, ticks, zombie))
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn all_processes() -> Vec<(u32, u32, u32, u64, bool)> {
    let Some(out) = ps(&[
        "-A", "-o", "pid=", "-o", "pgid=", "-o", "ppid=", "-o", "stat=", "-o", "time=",
    ]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let [pid, group, ppid, stat, time] = f[..] else {
                return None;
            };
            Some((
                pid.parse().ok()?,
                group.parse().ok()?,
                ppid.parse().ok()?,
                ps_ticks(time).unwrap_or(0),
                stat.starts_with('Z'),
            ))
        })
        .collect()
}

fn tree_of(pgid: u32, all: &[(u32, u32, u32, u64, bool)]) -> GroupSample {
    let mut s = GroupSample::default();
    let mut members: BTreeSet<u32> = all
        .iter()
        .filter(|p| p.1 == pgid && !p.4)
        .map(|p| p.0)
        .collect();
    loop {
        let more: Vec<u32> = all
            .iter()
            .filter(|p| !p.4 && !members.contains(&p.0) && members.contains(&p.2))
            .map(|p| p.0)
            .collect();
        if more.is_empty() {
            break;
        }
        members.extend(more);
    }
    for p in all.iter().filter(|p| members.contains(&p.0)) {
        s.cpu_ticks += p.3;
        s.pids.insert(p.0);
    }
    s
}

/// Without `/proc` (macOS), `ps` lists every process with its group, state
/// and CPU time; a failed `ps` gives an empty sample (only output then
/// counts as activity).
#[cfg(not(target_os = "linux"))]
pub fn sample_group(pgid: u32) -> GroupSample {
    ps(&[
        "-A", "-o", "pid=", "-o", "pgid=", "-o", "stat=", "-o", "time=",
    ])
    .map(|out| parse_ps(&out, pgid))
    .unwrap_or_default()
}

/// Whether this OS gives machine-wide CPU counters and orphan reaping:
/// without them, experiments cannot see load from other processes, arms
/// stopped from outside, or processes a trial left behind.
pub const MACHINE_COUNTERS: bool = cfg!(target_os = "linux");

/// Members of group `pgid` in `ps -o pid=,pgid=,stat=,time=` output; CPU
/// time (`[[H:]M:]S.cc`) in ticks of 10 ms, as on Linux. Zombies are out.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn parse_ps(out: &str, pgid: u32) -> GroupSample {
    let mut s = GroupSample::default();
    for line in out.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let [pid, group, stat, time] = f[..] else {
            continue;
        };
        let (Ok(pid), Ok(group)) = (pid.parse::<u32>(), group.parse::<u32>()) else {
            continue;
        };
        if group != pgid || stat.starts_with('Z') {
            continue;
        }
        s.cpu_ticks += ps_ticks(time).unwrap_or(0);
        s.pids.insert(pid);
    }
    s
}

/// Returns (pgrp, utime + stime, is_zombie) from `/proc/<pid>/stat`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_stat(stat: &str) -> Option<(u32, u64, bool)> {
    // The command name may contain spaces and parentheses; fields resume
    // after the last ')'.
    let rest = &stat[stat.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // rest[0] = state (field 3), pgrp = field 5, utime = 14, stime = 15,
    // cutime = 16, cstime = 17: the CPU of children a member already
    // reaped counts too (a shell loop of short-lived commands looked idle
    // and stalled, because each command was gone before a sample saw it).
    let state = f.first()?;
    let pgrp = f.get(2)?.parse().ok()?;
    let mut ticks = 0;
    for i in 11..=14 {
        ticks += f.get(i)?.parse::<u64>().ok()?;
    }
    Some((pgrp, ticks, *state == "Z" || *state == "X"))
}

pub fn signal_group(pgid: u32, sig: Signal) {
    if let Some(pid) = Pid::from_raw(pgid as i32) {
        // ESRCH (group already gone) is the expected race; ignore it.
        let _ = rustix::process::kill_process_group(pid, sig);
    }
}

/// Whether `pid` was started for `run`: prova gives every command it runs
/// `PROVA_RUN_ID=<run>`, and children inherit it. A stored process group id
/// can be reused by an unrelated group once the run's group is empty, so
/// nothing is killed by stored id alone.
#[cfg(target_os = "linux")]
pub fn belongs_to_run(pid: u32, run: &str) -> bool {
    let want = format!("PROVA_RUN_ID={run}");
    std::fs::read(format!("/proc/{pid}/environ"))
        .is_ok_and(|env| env.split(|b| *b == 0).any(|v| v == want.as_bytes()))
}

#[cfg(not(target_os = "linux"))]
pub fn belongs_to_run(pid: u32, run: &str) -> bool {
    let want = format!("PROVA_RUN_ID={run}");
    ps(&["-wwE", "-p", &pid.to_string(), "-o", "command="])
        .is_some_and(|out| out.split_whitespace().any(|v| v == want))
}

/// SIGKILL each process of group `pgid` that belongs to `run`; returns how
/// many were signalled.
pub fn kill_run_processes(pgid: u32, run: &str) -> usize {
    let mut n = 0;
    // The tree, not only the group: a child that started its own group
    // still carries the run's id.
    for pid in sample_tree(pgid).pids {
        if belongs_to_run(pid, run)
            && let Some(p) = Pid::from_raw(pid as i32)
        {
            let _ = rustix::process::kill_process(p, Signal::KILL);
            n += 1;
        }
    }
    n
}

/// Milliseconds since boot, the clock process start times are measured in
/// (Linux, `CLOCK_BOOTTIME`); None elsewhere. Not `/proc/uptime`: its 10 ms
/// truncation put the runner's own start after its first output line
/// (round 12 CI).
#[cfg(target_os = "linux")]
pub fn uptime_ms() -> Option<u64> {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Boottime);
    Some(t.tv_sec as u64 * 1000 + t.tv_nsec as u64 / 1_000_000)
}

#[cfg(not(target_os = "linux"))]
pub fn uptime_ms() -> Option<u64> {
    None
}

pub fn terminate_group(pgid: u32) {
    signal_group(pgid, Signal::TERM);
}

pub fn kill_group(pgid: u32) {
    signal_group(pgid, Signal::KILL);
}

/// True unless the process certainly does not exist. A process we may not
/// signal (EPERM) is alive.
pub fn alive(pid: u32) -> bool {
    Pid::from_raw(pid as i32).is_some_and(|p| {
        !matches!(
            rustix::process::test_kill_process(p),
            Err(rustix::io::Errno::SRCH)
        )
    })
}

/// Whether `pid` is the supervisor of `run`. A pid alone is not enough: in
/// containers and after reboots pids are reused, so a dead supervisor's pid
/// can belong to an unrelated process. Where `/proc` exists, the process
/// must be `prova __supervise ... <run>`.
pub fn supervises(pid: u32, run: &str) -> bool {
    match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(cmd) => {
            let args: Vec<&[u8]> = cmd.split(|b| *b == 0).collect();
            args.contains(&b"__supervise".as_slice()) && args.contains(&run.as_bytes())
        }
        Err(_) if std::path::Path::new("/proc/self").exists() => false,
        // No /proc (macOS): ask `ps` for the command line; the pid alone
        // only if `ps` cannot run at all.
        Err(_) => match ps(&["-p", &pid.to_string(), "-o", "command="]) {
            Some(cmd) => {
                let args: Vec<&str> = cmd.split_whitespace().collect();
                args.contains(&"__supervise") && args.contains(&run)
            }
            None => alive(pid) && ps(&["-p", "1", "-o", "pid="]).is_none(),
        },
    }
}

/// `ps` with these arguments: its output, or None when it fails or finds
/// nothing.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn ps(args: &[&str]) -> Option<String> {
    let o = std::process::Command::new("ps")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    (!out.trim().is_empty()).then_some(out)
}

/// CPU time in `ps` format (`[[H:]M:]S.cc`) as ticks of 10 ms.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn ps_ticks(time: &str) -> Option<u64> {
    let mut secs = 0.0;
    for part in time.trim().split(':') {
        secs = secs * 60.0 + part.parse::<f64>().ok()?;
    }
    Some((secs * 100.0).round() as u64)
}

/// Members still in group `pgid`, as `pid name`: what a workload left
/// running in its own group, found before the group is killed (round 11
/// on arm64: a background process that had not yet called setsid when its
/// shell exited was killed with the group and never reported).
pub fn left_in_group(pgid: u32) -> Vec<String> {
    sample_group(pgid)
        .pids
        .into_iter()
        .map(|p| format!("{p} {}", process_name(p)))
        .collect()
}

#[cfg(target_os = "linux")]
fn process_name(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

#[cfg(not(target_os = "linux"))]
fn process_name(pid: u32) -> String {
    ps(&["-p", &pid.to_string(), "-o", "comm="])
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

/// Makes this process the reaper of its orphaned descendants, so a process
/// that escapes its group (`setsid`) is still ours to find (round 8).
#[cfg(target_os = "linux")]
pub fn become_subreaper() {
    let _ = rustix::process::set_child_subreaper(Some(rustix::process::getpid()));
}

#[cfg(not(target_os = "linux"))]
pub fn become_subreaper() {}

/// Kills and reaps every child process of this one, returning `pid comm`
/// of each. Call when no child should be alive: what is found was left
/// behind by a workload.
#[cfg(target_os = "linux")]
pub fn reap_orphans() -> Vec<String> {
    let me = std::process::id();
    let mut found = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        let comm = stat[stat.find('(').map_or(0, |i| i + 1)..close].to_owned();
        let f: Vec<&str> = stat[close + 2..].split_whitespace().collect();
        let (Some(state), Some(ppid)) = (f.first(), f.get(1).and_then(|p| p.parse::<u32>().ok()))
        else {
            continue;
        };
        if ppid != me {
            continue;
        }
        if let Some(p) = Pid::from_raw(pid as i32) {
            if *state != "Z" {
                found.push(format!("{pid} {comm}"));
                // Its own group too: it may have started more.
                let _ = rustix::process::kill_process_group(p, Signal::KILL);
                let _ = rustix::process::kill_process(p, Signal::KILL);
            }
            let _ = rustix::process::waitpid(Some(p), rustix::process::WaitOptions::empty());
        }
    }
    found
}

#[cfg(not(target_os = "linux"))]
pub fn reap_orphans() -> Vec<String> {
    Vec::new()
}

/// CPU ticks the whole machine spent busy, and those this process's
/// waited-for children used: their difference over a trial is what other
/// processes used meanwhile (round 9: load started outside the trial's tree,
/// e.g. through systemd-run, slowed only the other arm).
#[cfg(target_os = "linux")]
pub fn cpu_ticks() -> (u64, u64) {
    let busy = std::fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|s| {
            let l = s.lines().next()?.to_owned();
            let f: Vec<u64> = l
                .split_whitespace()
                .skip(1)
                .filter_map(|x| x.parse().ok())
                .collect();
            // user nice system idle iowait irq softirq steal
            Some(
                f.iter()
                    .enumerate()
                    .filter(|(i, _)| *i != 3 && *i != 4)
                    .map(|(_, v)| v)
                    .sum(),
            )
        })
        .unwrap_or(0);
    (busy, children_ticks().unwrap_or(0))
}

#[cfg(not(target_os = "linux"))]
pub fn cpu_ticks() -> (u64, u64) {
    (0, 0)
}

/// CPU ticks of this process's waited-for children, when the OS says.
#[cfg(target_os = "linux")]
pub fn children_ticks() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rest = &s[s.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // cutime, cstime are fields 16 and 17 (index 13 and 14 here)
    Some(f.get(13)?.parse::<u64>().ok()? + f.get(14)?.parse::<u64>().ok()?)
}

/// macOS has no unprivileged way to read children's CPU time without
/// `getrusage` (unsafe FFI): not measured, so checks built on it are
/// skipped rather than guessed (`ps -S` did not include them).
#[cfg(not(target_os = "linux"))]
pub fn children_ticks() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stat_with_awkward_command_names() {
        let line =
            "1234 (my (weird) cmd) S 1 4321 4321 0 -1 4194304 100 0 0 0 7 5 0 0 20 0 1 0 1 1 1";
        assert_eq!(parse_stat(line), Some((4321, 12, false)));
        let reaped = "7 (sh) S 1 7 7 0 -1 0 0 0 0 0 1 1 30 20 20 0 1 0 1 1 1";
        assert_eq!(parse_stat(reaped), Some((7, 52, false)));
        let zombie = "9 (x) Z 1 9 9 0 -1 0 0 0 0 0 1 1 0 0 20 0 1 0 1 1 1";
        assert_eq!(parse_stat(zombie), Some((9, 2, true)));
    }

    #[test]
    fn parses_ps_output() {
        let out = "  101   100 S      0:01.25\n  102   100 Z      0:00.50\n  103   100 R+  1:02:03.04\n  104   200 S      9:59.99\ngarbage\n";
        let s = parse_ps(out, 100);
        assert_eq!(s.pids, [101, 103].into_iter().collect());
        assert_eq!(s.cpu_ticks, 125 + 372_304);
        assert_eq!(ps_ticks(" 0:00.07\n"), Some(7));
        assert_eq!(ps_ticks("x"), None);
    }

    #[test]
    fn a_tree_includes_descendants_in_other_groups() {
        // 10 leads group 10; 11 (nextest's test) is its child in group 11;
        // 12 is 11's child; 20 is unrelated; 13 is a zombie child.
        let all = [
            (10, 10, 1, 5, false),
            (11, 11, 10, 7, false),
            (12, 11, 11, 1, false),
            (13, 10, 10, 9, true),
            (20, 20, 1, 50, false),
        ];
        let t = tree_of(10, &all);
        assert_eq!(t.pids, [10, 11, 12].into_iter().collect());
        assert_eq!(t.cpu_ticks, 13);
    }

    #[test]
    fn own_process_group_is_visible() {
        let pgid = rustix::process::getpgrp().as_raw_nonzero().get() as u32;
        let s = sample_group(pgid);
        assert!(s.pids.contains(&std::process::id()));
        assert!(alive(std::process::id()));
    }
}
