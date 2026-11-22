//! Process-group observation and control (ADR 0004, 0013 section 3).
//!
//! The process under test leads its own process group. Activity of the
//! whole group (CPU time, process set) is sampled from `/proc` on Linux;
//! elsewhere only output counts as activity.

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

#[cfg(not(target_os = "linux"))]
pub fn sample_group(_pgid: u32) -> GroupSample {
    GroupSample::default()
}

/// Returns (pgrp, utime + stime, is_zombie) from `/proc/<pid>/stat`.
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

pub fn alive(pid: u32) -> bool {
    Pid::from_raw(pid as i32).is_some_and(|p| rustix::process::test_kill_process(p).is_ok())
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

    #[cfg(target_os = "linux")]
    #[test]
    fn own_process_group_is_visible() {
        let pgid = rustix::process::getpgrp().as_raw_nonzero().get() as u32;
        let s = sample_group(pgid);
        assert!(s.pids.contains(&std::process::id()));
        assert!(alive(std::process::id()));
    }
}
