//! What a run's processes use, for test sizes (ADR 0025): the processes in
//! the run's group and the sockets they hold. A size is decided by these,
//! never by duration.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::Size;

/// Where a socket leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Peer {
    /// This machine: loopback, a Unix socket, or a listening socket.
    Local,
    /// Another host, by address.
    Remote(String),
}

/// One sample of a process group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    /// pid -> (parent pid, command line, arguments joined by spaces).
    pub procs: BTreeMap<u32, (u32, String)>,
    /// socket key (inode on Linux) -> (holding pid, peer).
    pub sockets: BTreeMap<u64, (u32, Peer)>,
    /// pid -> when it started, in ms since boot (Linux; ADR 0026).
    pub starts: BTreeMap<u32, u64>,
}

/// What one test was seen to use, beyond what it started with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seen {
    /// Samples taken inside the test (or the run's test phase): without
    /// one, nothing was observed and no size can be claimed (ADR 0026).
    pub samples: usize,
    /// Other processes the test ran (command names).
    pub processes: BTreeSet<String>,
    /// Sockets to this machine (count).
    pub local_sockets: usize,
    /// Hosts it connected to.
    pub remote: BTreeSet<String>,
}

impl Seen {
    pub fn size(&self) -> Size {
        if !self.remote.is_empty() {
            Size::Large
        } else if !self.processes.is_empty() || self.local_sockets > 0 {
            Size::Medium
        } else {
            Size::Small
        }
    }

    /// Why it has this size, in a few words. Small is the absence of
    /// evidence, and says so (ADR 0026).
    pub fn evidence(&self) -> String {
        let mut parts = Vec::new();
        if let Some(h) = self.remote.iter().next() {
            parts.push(format!("connected to {h}"));
        }
        if !self.processes.is_empty() {
            let names: Vec<&str> = self.processes.iter().take(3).map(String::as_str).collect();
            parts.push(format!("started {}", names.join(", ")));
        }
        if self.local_sockets > 0 {
            parts.push(format!("{} local socket(s)", self.local_sockets));
        }
        if parts.is_empty() {
            format!(
                "no other process or socket seen ({} sample{})",
                self.samples,
                if self.samples == 1 { "" } else { "s" }
            )
        } else {
            parts.join("; ")
        }
    }

    pub fn absorb(&mut self, other: &Seen) {
        self.samples += other.samples;
        self.processes.extend(other.processes.iter().cloned());
        self.local_sockets = self.local_sockets.max(other.local_sockets);
        self.remote.extend(other.remote.iter().cloned());
    }
}

/// The command name of a command line: its first word, without the path.
fn command_name(cmdline: &str) -> String {
    let first = cmdline.split(' ').next().unwrap_or("");
    first.rsplit('/').next().unwrap_or(first).to_owned()
}

/// Whether a process is the test's own: its arguments name the test (a
/// runner that starts one process per test, such as nextest, passes the
/// test's full name).
pub fn names_test(cmdline: &str, test: &str) -> bool {
    !test.is_empty()
        && cmdline.split(' ').skip(1).any(|a| {
            !a.is_empty()
                && !a.starts_with('-')
                && (a == test
                    // nextest names `crate::binary$path`; the process gets `path`.
                    || test.ends_with(&format!("${a}"))
                    || (a.contains("::") && test.ends_with(&format!("::{a}"))))
        })
}

/// The container client a command runs its tests through (`docker run`,
/// `podman exec`, `kubectl exec`, ...): the tests' own processes and
/// sockets are then out of sight (round 14: sizes claimed "small" for
/// tests run by `docker run`).
pub fn container_client(cmdline: &str) -> Option<&'static str> {
    let words: Vec<&str> = cmdline.split(' ').collect();
    // The executable, or the script an interpreter runs (`sh docker ...`).
    let (at, client) = words.iter().take(2).enumerate().find_map(|(i, w)| {
        let exe = w.rsplit('/').next()?;
        ["docker", "podman", "nerdctl", "kubectl"]
            .into_iter()
            .find(|c| exe == *c)
            .map(|c| (i, c))
    })?;
    let rest = &words[at + 1..];
    // The subcommand: `docker compose run|exec`, `docker run|exec`,
    // `kubectl exec|run|debug`; `logs`, `attach`, `cp` replay or copy
    // output, they run nothing (round 15: `docker compose logs` passed as a
    // container run, skipping the no-CPU check).
    let sub = rest
        .iter()
        .copied()
        .filter(|w| !w.starts_with('-'))
        .find(|w| *w != "compose" && *w != "container")?;
    // `docker start -a` runs a created container attached (round 16:
    // treated as a replay).
    let attached_start = sub == "start"
        && rest
            .iter()
            .any(|w| matches!(*w, "-a" | "--attach" | "-ai" | "-ia"));
    if !matches!(sub, "run" | "exec" | "debug") && !attached_start {
        return None;
    }
    // `docker exec box cat results.txt` prints a file: no test runs.
    let viewer = rest.iter().any(|w| {
        matches!(
            w.rsplit('/').next().unwrap_or(w),
            "cat" | "tail" | "head" | "less" | "more" | "tac"
        )
    });
    (!viewer).then_some(client)
}

/// Whether the client's command line starts a container (`docker run`),
/// the only kind whose container this run can name.
pub fn container_client_runs(cmdline: &str) -> bool {
    let words: Vec<&str> = cmdline.split(' ').collect();
    words.iter().take(2).enumerate().any(|(i, w)| {
        matches!(w.rsplit('/').next(), Some("docker" | "podman" | "nerdctl"))
            && words.get(i + 1) == Some(&"run")
    })
}

/// The container a `docker run` command line names: `--name X`, or the id
/// in its `--cidfile`. None when it names neither (not attributable).
pub fn container_of(cmdline: &str) -> Option<String> {
    let words: Vec<&str> = cmdline.split(' ').collect();
    let value = |flag: &str| {
        words.iter().enumerate().find_map(|(i, w)| {
            if *w == flag {
                words.get(i + 1).map(|v| (*v).to_owned())
            } else {
                w.strip_prefix(&format!("{flag}=")).map(str::to_owned)
            }
        })
    };
    if let Some(name) = value("--name") {
        return Some(name);
    }
    let file = value("--cidfile")?;
    let id = std::fs::read_to_string(file).ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

/// Runs `client args` and returns its stdout, abandoning a client that
/// takes longer than `secs`.
fn client_output(client: &str, args: &[&str], secs: u64) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(client)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = out.read_to_string(&mut text);
        text
    });
    let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let text = reader.join().ok()?;
                return status.success().then_some(text);
            }
            Ok(None) if std::time::Instant::now() < until => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Milliseconds since the epoch of an RFC 3339 UTC time as docker prints it
/// (`2026-11-24T10:00:00.123456789Z`).
fn rfc3339_ms(t: &str) -> Option<u64> {
    let (date, time) = t.split_once('T')?;
    let time = time.trim_end_matches('Z');
    let mut d = date.split('-').map(|n| n.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (hms, frac) = time.split_once('.').unwrap_or((time, ""));
    let mut h = hms.split(':').map(|n| n.parse::<u64>().ok());
    let (hh, mm, ss) = (h.next()??, h.next()??, h.next()??);
    let ms: u64 = format!("{frac:0<3}")[..3].parse().ok()?;
    // Days since 1970-01-01 (Howard Hinnant's civil-date algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days)
        .ok()
        .map(|d| ((d * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms)
}

/// The host directories the client's own `-v`/`--volume`/`--mount`
/// arguments bind, resolved against the run's directory. Named volumes
/// (no path) are left out.
pub fn client_mount_sources(cmdline: &str, cwd: &str) -> Vec<std::path::PathBuf> {
    let words: Vec<&str> = cmdline.split(' ').collect();
    let mut specs: Vec<&str> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if matches!(*w, "-v" | "--volume" | "--mount") {
            if let Some(v) = words.get(i + 1) {
                specs.push(v);
            }
        } else if let Some(v) = w.strip_prefix("--volume=").or(w.strip_prefix("--mount=")) {
            specs.push(v);
        } else if let Some(v) = w.strip_prefix("-v").filter(|v| v.contains(':')) {
            specs.push(v);
        }
    }
    let mut out = Vec::new();
    for spec in specs {
        let src = if spec.contains("source=") || spec.contains("src=") {
            spec.split(',').find_map(|kv| {
                kv.strip_prefix("source=")
                    .or_else(|| kv.strip_prefix("src="))
            })
        } else {
            spec.split(':').next()
        };
        let Some(src) = src.map(|s| s.trim_matches('"')) else {
            continue;
        };
        // A named volume has no path; a bind mount starts with `/`, `.` or `~`.
        if !(src.starts_with('/') || src.starts_with('.')) {
            continue;
        }
        let path = std::path::Path::new(src);
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::path::Path::new(cwd).join(path)
        };
        out.push(normalize(&abs));
    }
    out
}

/// A path without `.` and `..` components, without touching the disk.
fn normalize(p: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component::{CurDir, ParentDir};
    let mut out = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            CurDir => {}
            ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The words of the client's command after its image: the command the
/// container runs, when the client gave one. `Config.Image` of the
/// container names the image.
fn client_command_after<'a>(cmdline: &'a str, image: &str) -> Vec<&'a str> {
    let words: Vec<&str> = cmdline.split(' ').collect();
    words
        .iter()
        .position(|w| *w == image)
        .map(|at| words[at + 1..].to_vec())
        .unwrap_or_default()
}

/// Which of the inspected containers are the run's own: created after the
/// run started; mounting every directory the client's own `-v`/`--mount`
/// arguments name (when it named none, mounting the run's directory or a
/// parent of it); and running the client's command when it gave one.
pub fn select_run_containers(
    all: &[serde_json::Value],
    cmdline: &str,
    cwd: &str,
    started_ms: u64,
) -> Vec<String> {
    let named = client_mount_sources(cmdline, cwd);
    let cwd = std::path::Path::new(cwd);
    all.iter()
        .filter(|c| {
            let created = c["Created"].as_str().and_then(rfc3339_ms);
            created.is_some_and(|t| t >= started_ms)
        })
        .filter(|c| {
            let sources: Vec<std::path::PathBuf> = c["Mounts"]
                .as_array()
                .map(|ms| {
                    ms.iter()
                        .filter(|m| m["Type"].as_str().is_none_or(|t| t == "bind"))
                        .filter_map(|m| m["Source"].as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| normalize(std::path::Path::new(s)))
                        .collect()
                })
                .unwrap_or_default();
            if named.is_empty() {
                sources.iter().any(|s| cwd.starts_with(s))
            } else {
                named.iter().all(|n| sources.iter().any(|s| s == n))
            }
        })
        .filter(|c| {
            let Some(image) = c["Config"]["Image"].as_str() else {
                return true;
            };
            let wanted = client_command_after(cmdline, image);
            if wanted.is_empty() {
                return true;
            }
            let Some(path) = c["Path"].as_str() else {
                return true;
            };
            let mut ran = vec![path.to_owned()];
            ran.extend(
                c["Args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|a| a.as_str().map(str::to_owned)),
            );
            ran.iter().map(String::as_str).eq(wanted.iter().copied())
        })
        .filter_map(|c| c["Id"].as_str())
        .map(|id| id.chars().take(12).collect())
        .collect()
}

/// Running containers that look like the run's own, when its client was
/// given no `--name` or `--cidfile` (see `select_run_containers`).
/// Bounded: a client that hangs yields nothing. Ids, shortest form.
pub fn containers_of_run(client: &str, cmdline: &str, cwd: &str, started_ms: u64) -> Vec<String> {
    let Some(ids) = client_output(client, &["ps", "-q"], 3) else {
        return Vec::new();
    };
    let ids: Vec<&str> = ids.split_whitespace().collect();
    if ids.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["inspect"];
    args.extend(ids);
    let Some(json) = client_output(client, &args, 3) else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Array(all)) = serde_json::from_str::<serde_json::Value>(&json) else {
        return Vec::new();
    };
    select_run_containers(&all, cmdline, cwd, started_ms)
}

/// Kills a container by name or id with its client; true when it did.
/// Bounded: a client that hangs is abandoned.
pub fn kill_container(client: &str, target: &str) -> bool {
    use std::process::{Command, Stdio};
    let Ok(mut child) = Command::new(client)
        .args(["kill", target])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < until => {
                std::thread::yield_now();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Whether a process is a test runner's process for a single test
/// (libtest's `--exact <name>`, as nextest starts each test).
pub fn runs_one_test(cmdline: &str) -> bool {
    cmdline.split(' ').skip(1).any(|a| a == "--exact")
}

/// Whether a process is the test runner's own rather than something a
/// test started: one test's process (`--exact`), a Go test binary
/// (`-test.` flags) or a cargo test binary (`target/…/deps/`).
pub fn runner_process(cmdline: &str) -> bool {
    // Build and test tooling a framework starts on its own: MSBuild nodes,
    // the C# compiler server, VSTest's test host, Surefire's forked JVM,
    // Gradle's workers. Their children still count (round 13: every Java
    // and C# test was bounded by its own test host).
    const TOOLING: &[&str] = &[
        "/nodemode:",
        "VBCSCompiler",
        "testhost",
        "vstest.console",
        "surefirebooter",
        "surefire/",
        "GradleWorkerMain",
        "GradleDaemon",
    ];
    TOOLING.iter().any(|t| cmdline.contains(t))
        || runs_one_test(cmdline)
        || cmdline.split(' ').any(|a| a.starts_with("-test."))
        || cmdline
            .split(' ')
            .next()
            .is_some_and(|exe| exe.contains("/target/") && exe.contains("/deps/"))
}

/// What a test used that started inside its window, `[from, to]` in ms
/// since boot (ADR 0026): processes by their start time (a process the
/// test started at once counts, though it was there when its start line
/// was read), except the runner's own. Sockets have no start time:
/// `seen_new` covers them.
/// `runner_since`: when the run's first output line arrived; a process
/// that started before it was set up before anything was printed (the
/// script or runner itself), never by a test.
pub fn started_within(now: &Usage, from: u64, to: u64, runner_since: u64) -> BTreeSet<String> {
    now.procs
        .iter()
        .filter(|(pid, (_, cmd))| {
            now.starts
                .get(pid)
                .is_some_and(|t| *t + 10 >= from && *t + 10 <= to && *t >= runner_since)
                && !runner_process(cmd)
        })
        .map(|(_, (_, cmd))| command_name(cmd))
        .filter(|n| !n.is_empty())
        .collect()
}

/// Pids in `usage` that descend from `root` (not `root` itself).
fn descendants(usage: &Usage, root: u32) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    let mut frontier = vec![root];
    while let Some(p) = frontier.pop() {
        for (pid, (ppid, _)) in &usage.procs {
            if *ppid == p && out.insert(*pid) {
                frontier.push(*pid);
            }
        }
    }
    out
}

/// What a test whose own process is `own` uses in `now`: its descendants
/// and the sockets it and they hold.
pub fn seen_in_subtree(now: &Usage, own: u32) -> Seen {
    let tree = descendants(now, own);
    let mut seen = Seen::default();
    for pid in &tree {
        if let Some((_, cmd)) = now.procs.get(pid) {
            seen.processes.insert(command_name(cmd));
        }
        seen.processes.remove("");
    }
    count_sockets(now, |pid| pid == own || tree.contains(&pid), &mut seen);
    seen
}

/// What in `now` was never seen before the test started: `pids` and
/// `sockets` are everything seen up to its start.
pub fn seen_new(pids: &BTreeSet<u32>, sockets: &BTreeSet<u64>, now: &Usage) -> Seen {
    let base = Usage {
        procs: pids.iter().map(|p| (*p, (0, String::new()))).collect(),
        sockets: sockets.iter().map(|k| (*k, (0, Peer::Local))).collect(),
        starts: BTreeMap::new(),
    };
    seen_since(&base, now, None)
}

/// What appeared in `now` since `base` (the group when the test started),
/// apart from the test's own process when it has one.
pub fn seen_since(base: &Usage, now: &Usage, own: Option<u32>) -> Seen {
    let mut seen = Seen::default();
    for (pid, (_, cmd)) in &now.procs {
        if !base.procs.contains_key(pid) && Some(*pid) != own {
            seen.processes.insert(command_name(cmd));
        }
    }
    // Unnamed (caught mid-start): still a process, shown as such.
    if seen.processes.remove("") {
        seen.processes.insert("a process".into());
    }
    let new: BTreeSet<u64> = now
        .sockets
        .keys()
        .filter(|k| !base.sockets.contains_key(k))
        .copied()
        .collect();
    let fresh = Usage {
        procs: BTreeMap::new(),
        sockets: now
            .sockets
            .iter()
            .filter(|(k, _)| new.contains(k))
            .map(|(k, v)| (*k, v.clone()))
            .collect(),
        starts: BTreeMap::new(),
    };
    count_sockets(&fresh, |_| true, &mut seen);
    seen
}

fn count_sockets(usage: &Usage, holder: impl Fn(u32) -> bool, seen: &mut Seen) {
    let mut local = 0;
    for (pid, peer) in usage.sockets.values() {
        if !holder(*pid) {
            continue;
        }
        match peer {
            Peer::Local => local += 1,
            Peer::Remote(h) => {
                seen.remote.insert(h.clone());
            }
        }
    }
    seen.local_sockets = seen.local_sockets.max(local);
}

/// Peer of one `/proc/net/{tcp,udp}[6]` row's remote address column:
/// none for an unconnected or listening socket.
fn peer_of(rem: &str) -> Peer {
    let Some((addr, port)) = rem.split_once(':') else {
        return Peer::Local;
    };
    if addr.chars().all(|c| c == '0') || port == "0000" {
        return Peer::Local;
    }
    let bytes: Vec<u8> = (0..addr.len() / 2)
        .filter_map(|i| u8::from_str_radix(&addr[2 * i..2 * i + 2], 16).ok())
        .collect();
    // Each 32-bit word is in host (little-endian) order.
    let mut b = Vec::with_capacity(bytes.len());
    for w in bytes.chunks(4) {
        b.extend(w.iter().rev());
    }
    let port = u16::from_str_radix(port, 16).unwrap_or(0);
    match b.len() {
        4 if b[0] == 127 => Peer::Local,
        4 => Peer::Remote(format!("{}.{}.{}.{}:{port}", b[0], b[1], b[2], b[3])),
        16 => {
            let loopback = b[..15].iter().all(|x| *x == 0) && b[15] == 1;
            let mapped_v4 = b[..10].iter().all(|x| *x == 0) && b[10] == 0xff && b[11] == 0xff;
            if loopback || (mapped_v4 && b[12] == 127) {
                Peer::Local
            } else if mapped_v4 {
                Peer::Remote(format!("{}.{}.{}.{}:{port}", b[12], b[13], b[14], b[15]))
            } else {
                let groups: Vec<String> = b
                    .chunks(2)
                    .map(|g| format!("{:x}", u16::from_be_bytes([g[0], g[1]])))
                    .collect();
                Peer::Remote(format!("[{}]:{port}", groups.join(":")))
            }
        }
        _ => Peer::Local,
    }
}

/// inode -> peer from the text of `/proc/net/{tcp,tcp6,udp,udp6}`.
pub fn parse_inet_table(text: &str) -> BTreeMap<u64, Peer> {
    let mut out = BTreeMap::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if let (Some(rem), Some(inode)) = (f.get(2), f.get(9).and_then(|i| i.parse::<u64>().ok()))
            && inode != 0
        {
            out.insert(inode, peer_of(rem));
        }
    }
    out
}

/// Inodes listed in `/proc/net/unix`.
pub fn parse_unix_table(text: &str) -> BTreeSet<u64> {
    text.lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().nth(6)?.parse().ok())
        .filter(|i| *i != 0)
        .collect()
}

#[cfg(target_os = "linux")]
pub fn sample(pgid: u32) -> Usage {
    let mut u = Usage::default();
    let pids = crate::process::sample_tree(pgid).pids;
    let mut inodes: BTreeMap<u64, u32> = BTreeMap::new();
    for pid in &pids {
        let cmd = std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|b| {
                b.split(|c| *c == 0)
                    .filter(|a| !a.is_empty())
                    .map(|a| String::from_utf8_lossy(a).into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        // A process caught while starting or exiting has no command line yet.
        let cmd = if cmd.is_empty() {
            std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .map(|c| c.trim().to_owned())
                .unwrap_or_default()
        } else {
            cmd
        };
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let fields: Vec<&str> = stat
            .rfind(')')
            .map(|i| stat[i + 2..].split_whitespace().collect())
            .unwrap_or_default();
        let ppid = fields.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
        // Field 22, start time in clock ticks since boot; USER_HZ is 100 on
        // Linux.
        if let Some(t) = fields.get(19).and_then(|v| v.parse::<u64>().ok()) {
            u.starts.insert(*pid, t * 10);
        }
        u.procs.insert(*pid, (ppid, cmd));
        if let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) {
            for fd in fds.flatten() {
                if let Ok(target) = std::fs::read_link(fd.path())
                    && let Some(n) = target
                        .to_str()
                        .and_then(|t| t.strip_prefix("socket:["))
                        .and_then(|t| t.strip_suffix(']'))
                        .and_then(|t| t.parse::<u64>().ok())
                {
                    inodes.insert(n, *pid);
                }
            }
        }
    }
    if inodes.is_empty() {
        return u;
    }
    // The group's own network namespace: read the tables through a member.
    let Some(any) = pids.iter().next() else {
        return u;
    };
    let mut peers = BTreeMap::new();
    for t in ["tcp", "tcp6", "udp", "udp6"] {
        if let Ok(text) = std::fs::read_to_string(format!("/proc/{any}/net/{t}")) {
            peers.extend(parse_inet_table(&text));
        }
    }
    let unix = std::fs::read_to_string(format!("/proc/{any}/net/unix"))
        .map(|t| parse_unix_table(&t))
        .unwrap_or_default();
    for (inode, pid) in inodes {
        // Netlink and other kernel sockets are not network use.
        if let Some(peer) = peers.get(&inode) {
            u.sockets.insert(inode, (pid, peer.clone()));
        } else if unix.contains(&inode) {
            u.sockets.insert(inode, (pid, Peer::Local));
        }
    }
    u
}

/// macOS: processes from `ps`, sockets from `lsof` when it is installed.
#[cfg(not(target_os = "linux"))]
pub fn sample(pgid: u32) -> Usage {
    let mut u = Usage::default();
    let Some(out) = run(&[
        "ps", "-A", "-o", "pid=", "-o", "pgid=", "-o", "ppid=", "-o", "command=",
    ]) else {
        return u;
    };
    for line in out.lines() {
        let mut f = line.split_whitespace();
        let (Some(pid), Some(group), Some(ppid)) = (
            f.next().and_then(|v| v.parse::<u32>().ok()),
            f.next().and_then(|v| v.parse::<u32>().ok()),
            f.next().and_then(|v| v.parse::<u32>().ok()),
        ) else {
            continue;
        };
        let _ = group;
        u.procs.insert(pid, (ppid, f.collect::<Vec<_>>().join(" ")));
    }
    let tree = crate::process::sample_tree(pgid).pids;
    u.procs.retain(|pid, _| tree.contains(pid));
    if u.procs.is_empty() {
        return u;
    }
    let pids: Vec<String> = u.procs.keys().map(u32::to_string).collect();
    if let Some(out) = run(&[
        "lsof",
        "-n",
        "-P",
        "-a",
        "-i",
        "-F",
        "pfn",
        "-p",
        &pids.join(","),
    ]) {
        u.sockets = parse_lsof(&out);
    }
    u
}

#[cfg(not(target_os = "linux"))]
fn run(args: &[&str]) -> Option<String> {
    let o = std::process::Command::new(args[0])
        .args(&args[1..])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Sockets from `lsof -F pfn` output: `p<pid>`, `f<fd>`, `n<name>` lines;
/// a name `a:1->b:2` is connected to `b`.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn parse_lsof(out: &str) -> BTreeMap<u64, (u32, Peer)> {
    let mut map = BTreeMap::new();
    let (mut pid, mut fd) = (0u32, 0u64);
    for line in out.lines() {
        let (tag, v) = line.split_at(line.len().min(1));
        match tag {
            "p" => pid = v.parse().unwrap_or(0),
            "f" => fd = v.parse().unwrap_or(0),
            "n" => {
                let peer = match v.split_once("->") {
                    Some((_, remote)) => {
                        let host = remote.rsplit_once(':').map_or(remote, |(h, _)| h);
                        let host = host.trim_start_matches('[').trim_end_matches(']');
                        if host == "localhost" || host.starts_with("127.") || host == "::1" {
                            Peer::Local
                        } else {
                            Peer::Remote(remote.to_owned())
                        }
                    }
                    None => Peer::Local,
                };
                map.insert((u64::from(pid) << 20) | fd, (pid, peer));
            }
            _ => {}
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inet_rows_are_classified_by_peer() {
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 11 1 0 100 0 0 10 0\n\
   1: 0100007F:9C40 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 12 1 0 20 4 30 10 -1\n\
   2: 0A00020F:9C41 0505A8C0:01BB 01 00000000:00000000 00:00000000 00000000  1000        0 13 1 0 20 4 30 10 -1\n";
        let t = parse_inet_table(tcp);
        assert_eq!(t[&11], Peer::Local, "listening");
        assert_eq!(t[&12], Peer::Local, "loopback");
        assert_eq!(t[&13], Peer::Remote("192.168.5.5:443".into()));
        let tcp6 = "header\n\
   0: 00000000000000000000000001000000:1F90 00000000000000000000000001000000:9C40 01 0:0 0:0 0 1000 0 21 1\n\
   1: 0000000000000000FFFF00000100007F:1F90 0000000000000000FFFF00000100007F:9C40 01 0:0 0:0 0 1000 0 22 1\n\
   2: 0000000000000000FFFF00000A00020F:1F90 0000000000000000FFFF00000505A8C0:01BB 01 0:0 0:0 0 1000 0 23 1\n";
        let t = parse_inet_table(tcp6);
        assert_eq!(t[&21], Peer::Local, "::1");
        assert_eq!(t[&22], Peer::Local, "::ffff:127.0.0.1");
        assert_eq!(t[&23], Peer::Remote("192.168.5.5:443".into()));
        let unix = "Num RefCount Protocol Flags Type St Inode Path\n0000: 00000002 00000000 00010000 0001 01 31 /tmp/s\n";
        assert!(parse_unix_table(unix).contains(&31));
    }

    #[test]
    fn lsof_names_are_classified() {
        let out = "p42\nf3\nn127.0.0.1:5000->127.0.0.1:5432\nf4\nn10.0.0.2:5001->93.184.216.34:443\nf5\nn*:8080\n";
        let m = parse_lsof(out);
        let peers: Vec<&Peer> = m.values().map(|(_, p)| p).collect();
        assert_eq!(peers.iter().filter(|p| **p == &Peer::Local).count(), 2);
        assert!(peers.contains(&&Peer::Remote("93.184.216.34:443".into())));
    }

    fn usage(procs: &[(u32, u32, &str)], sockets: &[(u64, u32, Peer)]) -> Usage {
        Usage {
            procs: procs
                .iter()
                .map(|(p, pp, c)| (*p, (*pp, (*c).to_owned())))
                .collect(),
            sockets: sockets
                .iter()
                .map(|(k, p, s)| (*k, (*p, s.clone())))
                .collect(),
            starts: BTreeMap::new(),
        }
    }

    #[test]
    fn sizes_follow_what_was_used_not_time() {
        let base = usage(&[(10, 1, "cargo test"), (11, 10, "target/t")], &[]);
        // Nothing new: small.
        assert_eq!(seen_since(&base, &base, None).size(), Size::Small);
        // A child process: medium, named.
        let now = usage(
            &[
                (10, 1, "cargo test"),
                (11, 10, "target/t"),
                (12, 11, "/usr/bin/postgres -D x"),
            ],
            &[],
        );
        let s = seen_since(&base, &now, None);
        assert_eq!(s.size(), Size::Medium);
        assert_eq!(s.evidence(), "started postgres");
        // A loopback socket: medium; another host: large.
        let now = usage(
            &[(10, 1, "cargo test"), (11, 10, "target/t")],
            &[(5, 11, Peer::Local)],
        );
        assert_eq!(seen_since(&base, &now, None).size(), Size::Medium);
        let now = usage(
            &[(10, 1, "cargo test"), (11, 10, "target/t")],
            &[(6, 11, Peer::Remote("10.1.1.1:80".into()))],
        );
        assert_eq!(seen_since(&base, &now, None).size(), Size::Large);
        // The test's own process (one per test) is not an extra process.
        let now = usage(
            &[
                (10, 1, "cargo test"),
                (13, 10, "t --exact m::a --nocapture"),
            ],
            &[],
        );
        assert_eq!(seen_since(&base, &now, Some(13)).size(), Size::Small);
    }

    #[test]
    fn a_tests_own_process_owns_its_subtree() {
        let now = usage(
            &[
                (10, 1, "nextest"),
                (20, 10, "t --exact m::a"),
                (21, 20, "sh -c redis-server"),
                (30, 10, "t --exact m::b"),
            ],
            &[
                (7, 21, Peer::Local),
                (8, 30, Peer::Remote("1.2.3.4:80".into())),
            ],
        );
        assert!(names_test("t --exact m::a", "m::a"));
        assert!(!names_test("t --exact m::ab", "m::a"));
        assert!(names_test("t --exact m::a --nocapture", "core::bin$m::a"));
        assert!(names_test("t --exact top_level", "cli::sizes$top_level"));
        assert!(
            !names_test("t --exact a", "core::bin$m::a"),
            "a bare leaf is ambiguous"
        );
        assert!(runs_one_test("t --exact m::a --nocapture") && !runs_one_test("cargo test"));
        let a = seen_in_subtree(&now, 20);
        assert_eq!(a.size(), Size::Medium);
        assert!(a.processes.contains("sh") && a.local_sockets == 1 && a.remote.is_empty());
        assert_eq!(seen_in_subtree(&now, 30).size(), Size::Large);
    }
}

#[cfg(test)]
mod container_tests {
    #[test]
    fn docker_times_are_read_as_epoch_milliseconds() {
        use super::rfc3339_ms as t;
        assert_eq!(t("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(t("2000-03-01T00:00:01.5Z"), Some(951_868_801_500));
        assert_eq!(t("2026-11-24T10:00:00.123456789Z"), Some(1_795_514_400_123));
        assert_eq!(t("nonsense"), None);
    }

    #[test]
    fn container_clients_are_recognized_by_what_they_do() {
        use super::container_client as c;
        assert_eq!(
            c("/usr/bin/docker run --rm -v /w:/w img dotnet test"),
            Some("docker")
        );
        assert_eq!(c("kubectl exec pod -- go test ./..."), Some("kubectl"));
        assert_eq!(c("/bin/sh /w/bin/docker run img"), Some("docker"));
        assert_eq!(c("docker ps"), None);
        // Round 15: replays and file viewers run nothing.
        assert_eq!(c("docker compose logs app"), None);
        assert_eq!(c("docker start -a box"), Some("docker"));
        assert_eq!(c("docker start box"), None);
        assert_eq!(c("docker exec box cat /out/results.txt"), None);
        assert_eq!(c("docker compose run --rm app pytest"), Some("docker"));
        assert_eq!(c("dockerd --host x"), None);
    }
}

#[cfg(test)]
mod container_choice_tests {
    use super::select_run_containers as pick;
    use serde_json::json;

    fn c(id: &str, created: &str, mounts: &[&str]) -> serde_json::Value {
        let ms: Vec<_> = mounts
            .iter()
            .map(|s| json!({"Type":"bind","Source":s}))
            .collect();
        json!({"Id":format!("{id}ffffffffff"),"Created":created,"Mounts":ms})
    }

    fn short(id: &str) -> String {
        format!("{id}ffffffffff").chars().take(12).collect()
    }

    #[test]
    fn a_container_mounting_a_parent_is_not_the_run_that_mounted_a_subdirectory() {
        // Round 17 B5: `docker run -v $PWD/src:/src` was cancelled by
        // killing an unrelated container that mounted $PWD.
        let all = vec![
            c("mine", "2999-01-01T00:00:01Z", &["/w/src"]),
            c("other", "2999-01-01T00:00:02Z", &["/w"]),
        ];
        let got = pick(
            &all,
            "docker run --rm -v /w/src:/src img sh -c unit",
            "/w",
            0,
        );
        assert_eq!(got, vec![short("mine")]);
    }

    #[test]
    fn every_named_mount_is_needed_and_relative_ones_resolve_against_the_run_dir() {
        let all = vec![
            c("one", "2999-01-01T00:00:00Z", &["/w/a"]),
            c("both", "2999-01-01T00:00:00Z", &["/w/a", "/w/b"]),
        ];
        let got = pick(&all, "docker run -v ./a:/a -v /w/b:/b img t", "/w", 0);
        assert_eq!(got, vec![short("both")]);
        let got = pick(
            &all,
            "docker run --mount type=bind,source=/w/a,target=/a img t",
            "/w",
            0,
        );
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn a_container_created_before_the_run_is_never_a_candidate() {
        let all = vec![c("dev", "2000-01-01T00:00:00Z", &["/w"])];
        assert!(pick(&all, "docker run img t", "/w", 2_000_000_000_000).is_empty());
        // Not even by the second of slack the old check allowed.
        let started = super::rfc3339_ms("2026-11-25T10:00:00.9Z").unwrap();
        let all = vec![c("dev", "2026-11-25T10:00:00.2Z", &["/w"])];
        assert!(pick(&all, "docker run img t", "/w", started).is_empty());
    }

    #[test]
    fn the_clients_command_must_match_when_it_gave_one() {
        let mut a = c("aaa", "2999-01-01T00:00:00Z", &["/w"]);
        a["Config"] = json!({"Image":"img"});
        a["Path"] = json!("sh");
        a["Args"] = json!(["-c", "unit"]);
        let mut b = c("bbb", "2999-01-01T00:00:00Z", &["/w"]);
        b["Config"] = json!({"Image":"img"});
        b["Path"] = json!("sleep");
        b["Args"] = json!(["999"]);
        let got = pick(&[a, b], "docker run --rm -v /w:/w img sh -c unit", "/w", 0);
        assert_eq!(got, vec![short("aaa")]);
    }
}
