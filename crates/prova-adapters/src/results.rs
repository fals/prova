//! Level 1: result files written during the run (ADR 0013 section 2).
//!
//! After the process exits, the workspace is searched for JUnit XML and TRX
//! files modified since the run started. No configuration needed for the
//! usual locations; `results` globs in `prova.toml` add more.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use prova_core::engine::{ExitReport, Observer};
use prova_core::model::{EventKind, Level};

use crate::junit;

/// Where frameworks commonly write results, relative to the root.
pub const DEFAULT_GLOBS: &[&str] = &[
    "**/junit*.xml",
    "**/*junit.xml",
    "**/TEST-*.xml",
    "**/test-results/**/*.xml",
    "**/test-reports/**/*.xml",
    "**/surefire-reports/*.xml",
    "**/failsafe-reports/*.xml",
    "**/*.trx",
    "target/nextest/**/*.xml",
    "**/report.xml",
];

/// Directories never searched. `target` is searched only for nextest output.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".prova",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".tox",
    ".gradle",
    ".cache",
];
/// The only subdirectories of a top-level `target/` that are searched.
const TARGET_RESULT_DIRS: &[&str] = &[
    "nextest",
    "surefire-reports",
    "failsafe-reports",
    "test-reports",
    "test-results",
];
const MAX_DEPTH: usize = 12;
const MAX_FILE: u64 = 64 * 1024 * 1024;
/// Clock slack between run start and file mtime.
const SLACK_MS: u64 = 2000;

pub struct ResultFiles {
    globs: Vec<String>,
}

impl ResultFiles {
    pub fn new(extra: &[String]) -> ResultFiles {
        let mut globs: Vec<String> = DEFAULT_GLOBS.iter().map(|s| s.to_string()).collect();
        globs.extend(extra.iter().cloned());
        ResultFiles { globs }
    }

    /// Files under `root` matching the globs, modified at or after `since_ms`.
    pub fn find(&self, root: &Path, since_ms: u64) -> Vec<PathBuf> {
        let mut out = Vec::new();
        walk(root, root, 0, &mut |rel, path| {
            if !self.globs.iter().any(|g| glob_match(g, rel)) {
                return;
            }
            let Ok(meta) = std::fs::metadata(path) else {
                return;
            };
            if !meta.is_file() || meta.len() > MAX_FILE {
                return;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            if mtime + SLACK_MS >= since_ms {
                out.push(path.to_path_buf());
            }
        });
        out.sort();
        out
    }
}

fn walk(root: &Path, dir: &Path, depth: usize, f: &mut dyn FnMut(&str, &Path)) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if ft.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            // Build output under target/ is huge: only enter result dirs.
            if rel.starts_with("target/")
                && depth == 1
                && !TARGET_RESULT_DIRS.contains(&name.as_ref())
            {
                continue;
            }
            walk(root, &path, depth + 1, f);
        } else if ft.is_file() {
            f(&rel, &path);
        }
    }
}

/// Minimal glob: `**` spans directories, `*` and `?` stay within one.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn segs(s: &str) -> Vec<&str> {
        s.split('/').filter(|x| !x.is_empty()).collect()
    }
    fn seg_match(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => seg_match(&p[1..], s) || (!s.is_empty() && seg_match(p, &s[1..])),
            (Some(b'?'), Some(_)) => seg_match(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => seg_match(&p[1..], &s[1..]),
            _ => false,
        }
    }
    fn go(p: &[&str], s: &[&str]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(&"**") => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            Some(seg) => {
                !s.is_empty() && seg_match(seg.as_bytes(), s[0].as_bytes()) && go(&p[1..], &s[1..])
            }
        }
    }
    go(&segs(pattern), &segs(path))
}

/// Regular files directly inside `dir`, sorted.
pub fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

impl ResultFiles {
    /// Parses result files into test events; unparseable files are skipped.
    pub fn ingest(&self, paths: Vec<PathBuf>) -> ExitReport {
        let mut report = ExitReport::default();
        for path in paths {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(cases) = junit::parse(&text) else {
                continue;
            };
            if cases.is_empty() {
                continue;
            }
            for case in cases {
                if let Some(out) = &case.output {
                    report.test_outputs.push((case.key.clone(), out.clone()));
                }
                for (i, a) in case.attempts.into_iter().enumerate() {
                    report.events.push(EventKind::TestFinished {
                        test: case.key.clone(),
                        attempt: i as u32 + 1,
                        outcome: a.outcome,
                        duration_ms: a.duration_ms,
                        message: a.message,
                        source: Level::ResultFiles,
                    });
                }
            }
            report.level = Some(Level::ResultFiles);
        }
        report
    }
}

impl Observer for ResultFiles {
    fn on_exit(&mut self, root: &Path, started_ms: u64) -> ExitReport {
        self.ingest(self.find(root, started_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("**/junit*.xml", "junit.xml"));
        assert!(glob_match("**/junit*.xml", "a/b/junit-report.xml"));
        assert!(glob_match(
            "**/TEST-*.xml",
            "target/surefire-reports/TEST-com.Foo.xml"
        ));
        assert!(glob_match(
            "target/nextest/**/*.xml",
            "target/nextest/ci/junit.xml"
        ));
        assert!(!glob_match("**/junit*.xml", "a/junit.json"));
        assert!(!glob_match("*.xml", "a/b.xml"));
        assert!(glob_match("**/*.trx", "TestResults/x.trx"));
    }

    #[test]
    fn finds_only_files_written_during_the_run() {
        let root = std::env::temp_dir().join(format!("prova-results-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("reports")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::write(root.join("reports/junit.xml"), "<testsuite/>").unwrap();
        std::fs::write(root.join("node_modules/x/junit.xml"), "<testsuite/>").unwrap();
        let rf = ResultFiles::new(&[]);
        let found = rf.find(&root, 0);
        assert_eq!(found, vec![root.join("reports/junit.xml")]);
        let future = prova_core::ids::now_ms() + 60_000;
        assert!(rf.find(&root, future).is_empty());
    }
}
