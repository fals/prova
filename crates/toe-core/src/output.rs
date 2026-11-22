//! Distilled output (ADR 0016).
//!
//! While a run is live its output goes to a spool file. When it ends, what
//! an agent needs is kept, compressed, in `toe.db`: an excerpt of the run's
//! output (head, error blocks, tail) and each failing test's own output.
//! Then the run's files are deleted. Kept output shares a per-repository
//! budget; the oldest is dropped first.

use std::collections::VecDeque;
use std::io::{BufRead, Read, Write};
use std::path::Path;

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::ids::now_ms;
use crate::store::{Result, Store, StoreError};

/// Lines kept from the start of a failed run.
const HEAD: usize = 30;
/// Lines kept from the end: failed and passed runs.
const TAIL_FAILED: usize = 80;
const TAIL_PASSED: usize = 20;
/// Context around a line that looks like an error.
const BEFORE: usize = 2;
const AFTER: usize = 10;
/// Upper bound of one excerpt and of one test's output, in bytes.
pub const EXCERPT_BYTES: usize = 64 * 1024;
/// Longest line kept.
const LINE_CHARS: usize = 2000;
/// Compressed output kept per repository.
pub const BUDGET_BYTES: i64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SectionKind {
    Head,
    Error,
    Tail,
}

/// Consecutive lines of the original output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Section {
    pub kind: SectionKind,
    /// 1-based line number of the first line.
    pub start: u64,
    pub lines: Vec<String>,
}

/// What is kept of a run's output.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Excerpt {
    pub total_lines: u64,
    pub total_bytes: u64,
    pub sections: Vec<Section>,
}

impl Excerpt {
    pub fn kept_lines(&self) -> u64 {
        self.sections.iter().map(|s| s.lines.len() as u64).sum()
    }

    /// The last `n` kept lines.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let all: Vec<&String> = self.sections.iter().flat_map(|s| &s.lines).collect();
        all[all.len().saturating_sub(n)..]
            .iter()
            .map(|l| l.to_string())
            .collect()
    }

    /// Text for humans, with the elided parts marked.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut next = 1;
        for s in &self.sections {
            if s.start > next {
                out.push_str(&format!("[... {} lines elided ...]\n", s.start - next));
            }
            if s.kind == SectionKind::Error {
                out.push_str(&format!("[line {}]\n", s.start));
            }
            for l in &s.lines {
                out.push_str(l);
                out.push('\n');
            }
            next = s.start + s.lines.len() as u64;
        }
        if self.total_lines >= next {
            out.push_str(&format!(
                "[... {} lines elided ...]\n",
                self.total_lines + 1 - next
            ));
        }
        out
    }
}

/// Does this line look like the explanation of a failure?
fn looks_like_error(line: &str) -> bool {
    let t = line.trim_start();
    const STARTS: &[&str] = &[
        "error",
        "Error",
        "ERROR",
        "FAIL",
        "--- FAIL",
        "fatal:",
        "Fatal",
        "panic:",
        "npm ERR!",
        "[ERROR]",
        "BUILD FAILED",
        "Traceback (most recent call last)",
        "E   ",
        "not ok ",
        "×",
        "✗",
        "Caused by:",
        "Unhandled",
    ];
    const CONTAINS: &[&str] = &[
        "panicked at",
        "error:",
        "error[",
        "Exception",
        "AssertionError",
        "Assertion failed",
        "undefined reference",
        "Segmentation fault",
        "core dumped",
        "FAILED",
    ];
    STARTS.iter().any(|p| t.starts_with(p)) || CONTAINS.iter().any(|p| t.contains(p))
}

fn clip(line: &str) -> String {
    match line.char_indices().nth(LINE_CHARS) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_owned(),
    }
}

/// Lines that mean something broke, even in a passing run (stricter than
/// `looks_like_error`, which also matches test names and log levels).
fn looks_like_crash(line: &str) -> bool {
    let t = line.trim_start();
    const STARTS: &[&str] = &[
        "Traceback (most recent call last)",
        "fatal:",
        "panic:",
        "npm ERR!",
        "error[",
        "error:",
        "Unhandled",
        "Segmentation fault",
    ];
    const CONTAINS: &[&str] = &[
        "panicked at",
        "core dumped",
        "ModuleNotFoundError",
        "ImportError",
    ];
    STARTS.iter().any(|p| t.starts_with(p)) || CONTAINS.iter().any(|p| t.contains(p))
}

/// Builds an excerpt in one pass, with bounded memory, as lines arrive.
#[derive(Debug, Default)]
pub struct Distiller {
    lines: u64,
    bytes: u64,
    head: Vec<String>,
    /// Recent lines (for the tail and the context before an error).
    recent: VecDeque<(u64, String)>,
    errors: Vec<Section>,
    error_bytes: usize,
    /// Lines of context still to add after the last error line.
    after: usize,
}

impl Distiller {
    pub fn push(&mut self, line: &str) {
        self.lines += 1;
        self.bytes += line.len() as u64 + 1;
        let n = self.lines;
        let line = clip(line.trim_end_matches(['\n', '\r']));
        if self.head.len() < HEAD {
            self.head.push(line.clone());
        }
        let budget_left = self.error_bytes < EXCERPT_BYTES / 2;
        if budget_left && (looks_like_error(&line) || self.after > 0) {
            if looks_like_error(&line) {
                self.after = AFTER;
            } else {
                self.after -= 1;
            }
            let extends = self
                .errors
                .last()
                .is_some_and(|s| s.start + s.lines.len() as u64 == n);
            if !extends {
                let before: Vec<(u64, String)> = self
                    .recent
                    .iter()
                    .rev()
                    .take(BEFORE)
                    .rev()
                    .filter(|(i, _)| {
                        self.errors
                            .last()
                            .is_none_or(|s| *i >= s.start + s.lines.len() as u64)
                    })
                    .cloned()
                    .collect();
                let start = before.first().map(|(i, _)| *i).unwrap_or(n);
                self.errors.push(Section {
                    kind: SectionKind::Error,
                    start,
                    lines: before.into_iter().map(|(_, l)| l).collect(),
                });
            }
            self.error_bytes += line.len() + 1;
            if let Some(s) = self.errors.last_mut() {
                s.lines.push(line.clone());
            }
        }
        self.recent.push_back((n, line));
        if self.recent.len() > TAIL_FAILED {
            self.recent.pop_front();
        }
    }

    /// First line that looks like a crash, with its line number.
    pub fn first_crash(&self) -> Option<(u64, String)> {
        self.first_matching(looks_like_crash)
    }

    /// First line that looks like an error, with its line number.
    pub fn first_error(&self) -> Option<(u64, String)> {
        self.first_matching(looks_like_error)
    }

    fn first_matching(&self, f: fn(&str) -> bool) -> Option<(u64, String)> {
        self.errors.iter().find_map(|s| {
            s.lines
                .iter()
                .enumerate()
                .find(|(_, l)| f(l))
                .map(|(i, l)| (s.start + i as u64, l.clone()))
        })
    }

    /// The excerpt: a passed run keeps only its last lines.
    pub fn finish(self, passed: bool) -> Excerpt {
        let tail_n = if passed { TAIL_PASSED } else { TAIL_FAILED };
        let tail: Vec<(u64, String)> = self
            .recent
            .iter()
            .skip(self.recent.len().saturating_sub(tail_n))
            .cloned()
            .collect();
        let tail_start = tail.first().map(|(i, _)| *i).unwrap_or(self.lines + 1);
        let mut sections = Vec::new();
        if !passed {
            let head: Vec<String> = self
                .head
                .into_iter()
                .take((tail_start.saturating_sub(1)) as usize)
                .collect();
            let head_end = head.len() as u64;
            if !head.is_empty() {
                sections.push(Section {
                    kind: SectionKind::Head,
                    start: 1,
                    lines: head,
                });
            }
            for mut s in self.errors {
                // Keep only what head and tail do not already show.
                let mut start = s.start;
                while start <= head_end && !s.lines.is_empty() {
                    s.lines.remove(0);
                    start += 1;
                }
                let end = start + s.lines.len() as u64;
                if end > tail_start {
                    let keep = tail_start.saturating_sub(start) as usize;
                    s.lines.truncate(keep);
                }
                if !s.lines.is_empty() {
                    s.start = start;
                    sections.push(s);
                }
            }
        }
        if !tail.is_empty() {
            sections.push(Section {
                kind: SectionKind::Tail,
                start: tail_start,
                lines: tail.into_iter().map(|(_, l)| l).collect(),
            });
        }
        let mut e = Excerpt {
            total_lines: self.lines,
            total_bytes: self.bytes,
            sections,
        };
        bound(&mut e);
        e
    }
}

/// Caps an excerpt at EXCERPT_BYTES, cutting error sections first.
fn bound(e: &mut Excerpt) {
    let size = |e: &Excerpt| -> usize {
        e.sections
            .iter()
            .flat_map(|s| &s.lines)
            .map(|l| l.len() + 1)
            .sum()
    };
    while size(e) > EXCERPT_BYTES {
        if let Some(i) = e
            .sections
            .iter()
            .rposition(|s| s.kind == SectionKind::Error)
        {
            e.sections.remove(i);
        } else if let Some(s) = e.sections.iter_mut().find(|s| s.lines.len() > 1) {
            s.lines.remove(0);
            s.start += 1;
        } else {
            break;
        }
    }
}

/// Distills a file of output (a spool or an old log).
pub fn distill_file(path: &Path, passed: bool) -> Option<Excerpt> {
    let f = std::fs::File::open(path).ok()?;
    let mut d = Distiller::default();
    let mut r = std::io::BufReader::new(f);
    let mut buf = Vec::new();
    while r.read_until(b'\n', &mut buf).ok()? > 0 {
        d.push(&String::from_utf8_lossy(&buf));
        buf.clear();
    }
    Some(d.finish(passed))
}

/// Bounds one test's output: the end matters most (the assertion, the panic).
pub fn bound_text(text: &str) -> String {
    if text.len() <= EXCERPT_BYTES {
        return text.to_owned();
    }
    let mut cut = text.len() - EXCERPT_BYTES;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    format!("[... {cut} bytes elided ...]\n{}", &text[cut..])
}

pub fn compress(data: &[u8]) -> Vec<u8> {
    let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
    // Writing to a Vec cannot fail.
    let _ = e.write_all(data);
    e.finish().unwrap_or_default()
}

pub fn decompress(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    DeflateDecoder::new(data).read_to_end(&mut out).ok()?;
    Some(out)
}

fn compress_file(path: &Path) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
    std::io::copy(&mut f, &mut e).ok()?;
    e.finish().ok()
}

// ------------------------------------------------------------------ store

/// Keeps a finished run's output: its excerpt, and the whole output if
/// asked (`toe run --keep-output`).
pub fn keep_run(
    store: &Store,
    run: &str,
    excerpt: &Excerpt,
    full_from: Option<&Path>,
) -> Result<()> {
    let json = serde_json::to_vec(excerpt).map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
    let full = full_from.and_then(compress_file);
    store.conn().execute(
        "INSERT OR REPLACE INTO run_output (run_id, excerpt, full, lines, bytes, pruned_ms, created_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
        params![
            run,
            compress(&json),
            full,
            excerpt.total_lines as i64,
            excerpt.total_bytes as i64,
            now_ms() as i64
        ],
    )?;
    Ok(())
}

pub fn keep_test(store: &Store, run: &str, test_id: &str, text: &str) -> Result<()> {
    store.conn().execute(
        // Only for a test the run recorded (the output of an unknown key is dropped).
        "INSERT OR REPLACE INTO test_output (run_id, test_id, output, created_ms)
         SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM tests WHERE id = ?2)",
        params![
            run,
            test_id,
            compress(bound_text(text).as_bytes()),
            now_ms() as i64
        ],
    )?;
    Ok(())
}

pub fn keep_comparison(
    store: &Store,
    comparison: &str,
    name: &str,
    excerpt: &Excerpt,
) -> Result<()> {
    let json = serde_json::to_vec(excerpt).map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
    store.conn().execute(
        "INSERT OR REPLACE INTO comparison_output (comparison, name, excerpt, created_ms) VALUES (?1, ?2, ?3, ?4)",
        params![comparison, name, compress(&json), now_ms() as i64],
    )?;
    Ok(())
}

fn excerpt_from(blob: Option<Vec<u8>>) -> Option<Excerpt> {
    serde_json::from_slice(&decompress(&blob?)?).ok()
}

/// What is kept of a finished run: its excerpt, or why there is none.
pub enum Kept {
    Excerpt(Excerpt),
    Pruned,
    Missing,
}

pub fn run_excerpt(store: &Store, run: &str) -> Result<Kept> {
    let row: Option<(Option<Vec<u8>>, Option<i64>)> = store
        .conn()
        .query_row(
            "SELECT excerpt, pruned_ms FROM run_output WHERE run_id = ?1",
            params![run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        None => Kept::Missing,
        Some((_, Some(_))) => Kept::Pruned,
        Some((blob, None)) => excerpt_from(blob)
            .map(Kept::Excerpt)
            .unwrap_or(Kept::Missing),
    })
}

/// The whole output, when the run was started with `--keep-output`.
pub fn run_full(store: &Store, run: &str) -> Result<Option<String>> {
    let blob: Option<Vec<u8>> = store
        .conn()
        .query_row(
            "SELECT full FROM run_output WHERE run_id = ?1 AND pruned_ms IS NULL",
            params![run],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(blob
        .and_then(|b| decompress(&b))
        .map(|b| String::from_utf8_lossy(&b).into_owned()))
}

/// Failing tests' own output in a run: (display name, text).
pub fn test_outputs(store: &Store, run: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = store.conn().prepare(
        "SELECT t.display, o.output FROM test_output o JOIN tests t ON t.id = o.test_id
         WHERE o.run_id = ?1 AND o.output IS NOT NULL ORDER BY t.display",
    )?;
    let rows = stmt
        .query_map(params![run], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?
        .filter_map(|r| r.ok())
        .filter_map(|(d, b)| decompress(&b).map(|t| (d, String::from_utf8_lossy(&t).into_owned())))
        .collect();
    Ok(rows)
}

pub fn comparison_excerpts(store: &Store, comparison: &str) -> Result<Vec<(String, Excerpt)>> {
    let mut stmt = store.conn().prepare(
        "SELECT name, excerpt FROM comparison_output WHERE comparison = ?1 AND excerpt IS NOT NULL ORDER BY created_ms",
    )?;
    let rows = stmt
        .query_map(params![comparison], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?
        .filter_map(|r| r.ok())
        .filter_map(|(n, b)| excerpt_from(Some(b)).map(|e| (n, e)))
        .collect();
    Ok(rows)
}

/// Drops the oldest kept output until the repository is within `budget`.
/// Run rows, results and statistics are never touched.
pub fn enforce_budget(store: &Store, budget: i64) -> Result<()> {
    let used = |c: &rusqlite::Connection| -> rusqlite::Result<i64> {
        c.query_row(
            "SELECT
               (SELECT COALESCE(SUM(LENGTH(excerpt) + COALESCE(LENGTH(full), 0)), 0) FROM run_output)
             + (SELECT COALESCE(SUM(LENGTH(output)), 0) FROM test_output)
             + (SELECT COALESCE(SUM(LENGTH(excerpt)), 0) FROM comparison_output)",
            [],
            |r| r.get(0),
        )
    };
    let c = store.conn();
    while used(c)? > budget {
        // Oldest run with something left to drop.
        let oldest: Option<(String, i64)> = c
            .query_row(
                "SELECT id, created_ms FROM (
                   SELECT run_id AS id, created_ms FROM run_output WHERE excerpt IS NOT NULL OR full IS NOT NULL
                   UNION ALL SELECT run_id, created_ms FROM test_output WHERE output IS NOT NULL
                   UNION ALL SELECT comparison, created_ms FROM comparison_output WHERE excerpt IS NOT NULL
                 ) ORDER BY created_ms LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((id, _)) = oldest else { break };
        let now = now_ms() as i64;
        c.execute(
            "UPDATE run_output SET excerpt = NULL, full = NULL, pruned_ms = ?2 WHERE run_id = ?1",
            params![id, now],
        )?;
        c.execute(
            "UPDATE test_output SET output = NULL WHERE run_id = ?1",
            params![id],
        )?;
        c.execute(
            "UPDATE comparison_output SET excerpt = NULL WHERE comparison = ?1",
            params![id],
        )?;
    }
    Ok(())
}

/// Removes files left by finished runs (crashed supervisors, toe before
/// ADR 0016): a run's output is distilled first when it has none yet.
/// Result-file copies and finished experiments' logs are deleted.
pub fn sweep(store: &Store) -> Result<()> {
    let runs = store.dir().join("runs");
    if let Ok(entries) = std::fs::read_dir(&runs) {
        for e in entries.flatten() {
            let id = e.file_name().to_string_lossy().into_owned();
            let state: Option<(String, Option<String>)> = store
                .conn()
                .query_row(
                    "SELECT status, outcome FROM runs WHERE id = ?1",
                    params![id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let terminal = match &state {
                Some((s, _)) => !matches!(s.as_str(), "created" | "running"),
                None => true, // no such run: nothing to keep
            };
            if !terminal {
                continue;
            }
            if let Some((_, outcome)) = &state {
                let has: bool = store.conn().query_row(
                    "SELECT EXISTS (SELECT 1 FROM run_output WHERE run_id = ?1)",
                    params![id],
                    |r| r.get(0),
                )?;
                if !has
                    && let Some(ex) = distill_file(
                        &e.path().join("output.log"),
                        outcome.as_deref() == Some("passed"),
                    )
                {
                    keep_run(store, &id, &ex, None)?;
                }
            }
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    let _ = std::fs::remove_dir_all(store.dir().join("artifacts"));
    let running: bool = store.conn().query_row(
        "SELECT EXISTS (SELECT 1 FROM comparisons WHERE status IN ('created', 'running'))",
        [],
        |r| r.get(0),
    )?;
    let worktrees = store.dir().join("worktrees");
    if !running && worktrees.exists() {
        if let Ok(entries) = std::fs::read_dir(&worktrees) {
            for e in entries.flatten() {
                let _ = std::process::Command::new("git")
                    .args(["worktree", "remove", "--force"])
                    .arg(e.path())
                    .current_dir(store.root())
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
        let _ = std::fs::remove_dir_all(&worktrees);
    }
    if let Ok(entries) = std::fs::read_dir(store.dir().join("experiments")) {
        for e in entries.flatten() {
            let id = e.file_name().to_string_lossy().into_owned();
            let open: bool = store.conn().query_row(
                "SELECT EXISTS (SELECT 1 FROM comparisons WHERE id = ?1 AND status IN ('created', 'running'))",
                params![id],
                |r| r.get(0),
            )?;
            if !open {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distill(text: &str, passed: bool) -> Excerpt {
        let mut d = Distiller::default();
        for l in text.lines() {
            d.push(l);
        }
        d.finish(passed)
    }

    #[test]
    fn short_output_is_kept_whole_once() {
        let e = distill("a\nerror: boom\nb", false);
        assert_eq!(e.render(), "a\nerror: boom\nb\n");
        assert_eq!(e.kept_lines(), 3);
    }

    #[test]
    fn long_failed_output_keeps_head_errors_and_tail() {
        let mut text: Vec<String> = (1..=1000).map(|i| format!("line {i}")).collect();
        text[499] = "error[E0308]: mismatched types".into();
        let e = distill(&text.join("\n"), false);
        let r = e.render();
        assert!(r.starts_with("line 1\n"), "{r}");
        assert!(
            r.contains("[line 498]\nline 498\nline 499\nerror[E0308]: mismatched types\nline 501"),
            "{r}"
        );
        assert!(r.contains("[... 467 lines elided ...]"), "{r}");
        assert!(r.ends_with("line 1000\n"));
        assert_eq!(e.total_lines, 1000);
        assert_eq!(e.kept_lines(), 30 + 13 + 80);
    }

    #[test]
    fn passed_run_keeps_only_its_last_lines() {
        let text: Vec<String> = (1..=100).map(|i| format!("ok {i} error-free")).collect();
        let e = distill(&text.join("\n"), true);
        assert_eq!(e.kept_lines(), 20);
        assert!(e.render().starts_with("[... 80 lines elided ...]\n"));
        assert_eq!(e.tail(2), vec!["ok 99 error-free", "ok 100 error-free"]);
    }

    #[test]
    fn excerpt_is_bounded() {
        let big = "x".repeat(1500);
        let text: Vec<String> = (0..10_000).map(|i| format!("error {i} {big}")).collect();
        let e = distill(&text.join("\n"), false);
        let size: usize = e
            .sections
            .iter()
            .flat_map(|s| &s.lines)
            .map(|l| l.len() + 1)
            .sum();
        assert!(size <= EXCERPT_BYTES, "{size}");
        assert_eq!(e.total_lines, 10_000);
    }

    #[test]
    fn compression_round_trips_and_text_is_bounded() {
        let t = "assert 16 == 4\n".repeat(10_000);
        let c = compress(t.as_bytes());
        assert!(c.len() * 20 < t.len());
        assert_eq!(decompress(&c).unwrap(), t.as_bytes());
        let b = bound_text(&t);
        assert!(b.len() < EXCERPT_BYTES + 64 && b.starts_with("[... "));
        assert!(b.ends_with("assert 16 == 4\n"));
    }
}
