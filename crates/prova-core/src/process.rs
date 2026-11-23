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
    // rest[0] = state (field 3), pgrp = field 5, utime = 14, stime = 15
    let state = f.first()?;
    let pgrp = f.get(2)?.parse().ok()?;
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    Some((pgrp, utime + stime, *state == "Z" || *state == "X"))
}

pub fn signal_group(pgid: u32, sig: Signal) {
    if let Some(pid) = Pid::from_raw(pgid as i32) {
        // ESRCH (group already gone) is the expected race; ignore it.
        let _ = rustix::process::kill_process_group(pid, sig);
    }
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

/// Members still in group `pgid`, as pids: where no subreaper collects
/// escaped processes (macOS), what a workload left running is found in its
/// own group before the group is killed. Linux finds them with
/// `reap_orphans`, so this is empty there.
#[cfg(target_os = "linux")]
pub fn left_in_group(_pgid: u32) -> Vec<String> {
    Vec::new()
}

#[cfg(not(target_os = "linux"))]
pub fn left_in_group(pgid: u32) -> Vec<String> {
    sample_group(pgid)
        .pids
        .into_iter()
        .map(|p| p.to_string())
        .collect()
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

/// No machine-wide counter here (0); this process's reaped children's CPU
/// from `ps -S` (own time plus exited children) minus its own time.
#[cfg(not(target_os = "linux"))]
pub fn cpu_ticks() -> (u64, u64) {
    (0, children_ticks().unwrap_or(0))
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

#[cfg(not(target_os = "linux"))]
pub fn children_ticks() -> Option<u64> {
    let me = std::process::id().to_string();
    let with = ps_ticks(&ps(&["-S", "-p", &me, "-o", "time="])?)?;
    let own = ps_ticks(&ps(&["-p", &me, "-o", "time="])?)?;
    Some(with.saturating_sub(own))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stat_with_awkward_command_names() {
        let line =
            "1234 (my (weird) cmd) S 1 4321 4321 0 -1 4194304 100 0 0 0 7 5 0 0 20 0 1 0 1 1 1";
        assert_eq!(parse_stat(line), Some((4321, 12, false)));
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
    fn own_process_group_is_visible() {
        let pgid = rustix::process::getpgrp().as_raw_nonzero().get() as u32;
        let s = sample_group(pgid);
        assert!(s.pids.contains(&std::process::id()));
        assert!(alive(std::process::id()));
    }
}
