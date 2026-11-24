//! SQLite store (ADR 0008). One file at `<root>/.prova/prova.db`.
//!
//! Invariants from `spec/run.qnt` are enforced here a second time by
//! triggers, so a buggy writer cannot record what the spec forbids.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{Event, EventKind, Level, Outcome, Status};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("run {0} not found")]
    RunNotFound(String),
    #[error("database schema version {0} is newer than this prova supports ({1})")]
    TooNew(i64, i64),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Ordered migrations; index + 1 is the `user_version` after applying.
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_runs.sql"),
    include_str!("migrations/0002_tests.sql"),
    include_str!("migrations/0003_experiments.sql"),
    include_str!("migrations/0004_origin.sql"),
    include_str!("migrations/0005_output.sql"),
    include_str!("migrations/0006_best_resets.sql"),
    include_str!("migrations/0007_immutable_samples.sql"),
    include_str!("migrations/0008_forgotten_runs.sql"),
    include_str!("migrations/0009_test_sizes.sql"),
];

pub struct Store {
    conn: Connection,
    /// Workspace root: where commands run and result files are searched.
    root: PathBuf,
    /// Data directory of this repository (ADR 0014).
    dir: PathBuf,
    repo: Option<crate::home::Repo>,
    fallback: bool,
}

/// A stored run, as read back.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunRow {
    pub id: String,
    pub suite: String,
    pub spec: serde_json::Value,
    pub cwd: String,
    pub code: String,
    pub status: Status,
    pub outcome: Option<Outcome>,
    pub level: Level,
    pub created_ms: u64,
    pub started_ms: Option<u64>,
    pub ended_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub reason: Option<String>,
    pub supervisor_pid: Option<u32>,
    pub cancel_requested: bool,
    pub fingerprint: Option<String>,
    /// Where the run happened: host, pod, node, actor (ADR 0014).
    pub origin: serde_json::Value,
}

/// Immutable inputs of a new run.
pub struct NewRun<'a> {
    pub id: &'a str,
    pub suite: &'a str,
    pub spec: &'a serde_json::Value,
    pub cwd: &'a str,
    pub code: &'a str,
    pub env: &'a serde_json::Value,
    pub machine: &'a serde_json::Value,
    /// Where the run happens: host, pod, node, actor.
    pub origin: &'a serde_json::Value,
    pub created_ms: u64,
}

impl Store {
    /// Opens the store of the repository `root` belongs to, in the central
    /// home (ADR 0014). A store left in `<root>/.prova` by an older prova is
    /// imported on first use.
    pub fn open(root: &Path) -> Result<Store> {
        let loc = crate::home::locate(root);
        let mut store = Store::open_dir(root, &loc.dir)?;
        store.repo = Some(loc.repo.clone());
        store.fallback = loc.fallback;
        if !loc.fallback {
            crate::home::register(&loc.repo, root);
            store.import_workspace_store()?;
        }
        Ok(store)
    }

    /// Opens a store in an explicit data directory.
    pub fn open_dir(root: &Path, dir: &Path) -> Result<Store> {
        private_dir(dir)?;
        let db = dir.join("prova.db");
        if !db.exists() {
            // Commands and output can carry secrets: owner-only (0600).
            drop(private_file(&db)?);
        }
        let conn = Connection::open(&db)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        // WAL needs a local filesystem; fall back silently if refused.
        let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            conn.execute_batch("PRAGMA journal_mode=DELETE")?;
        }
        conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
        let mut store = Store {
            conn,
            root: root.to_path_buf(),
            dir: dir.to_path_buf(),
            repo: None,
            fallback: false,
        };
        store.migrate()?;
        Ok(store)
    }

    /// Moves history from a workspace `.prova/prova.db` (prova before ADR 0014)
    /// into this store, then leaves a note. Run ids are random and test
    /// ids content hashes, so imports from several worktrees never clash.
    fn import_workspace_store(&mut self) -> Result<()> {
        let old_dir = self.root.join(".prova");
        let old_db = old_dir.join("prova.db");
        if !old_db.exists() || old_dir == self.dir {
            return Ok(());
        }
        // Bring the old file to the current schema first.
        drop(Store::open_dir(&self.root, &old_dir)?);
        const TABLES: &[&str] = &[
            "events",
            "attempts",
            "tests",
            "runs",
            "artifacts",
            "run_output",
            "test_output",
            "comparison_output",
            "samples",
            "trials",
            "decisions",
            "comparisons",
            "variants",
            "experiments",
        ];
        // Foreign keys are off for the copy (SQLite cannot defer them
        // reliably across INSERT OR IGNORE) and verified before commit.
        self.conn.execute_batch("PRAGMA foreign_keys = OFF")?;
        self.conn.execute(
            "ATTACH DATABASE ?1 AS old",
            params![old_db.to_string_lossy()],
        )?;
        let copied = (|| -> Result<()> {
            let tx = self.conn.unchecked_transaction()?;
            // Events before runs: the 'no events after end' trigger looks
            // up the run, which must not exist yet while importing.
            for t in TABLES {
                tx.execute_batch(&format!(
                    "INSERT OR IGNORE INTO main.{t} SELECT * FROM old.{t}"
                ))?;
            }
            let mut stmt = tx.prepare("PRAGMA main.foreign_key_check")?;
            let broken = stmt.query_map([], |_| Ok(()))?.count();
            drop(stmt);
            if broken > 0 {
                return Err(StoreError::Io(std::io::Error::other(format!(
                    "import of {} left {broken} dangling references; nothing imported",
                    old_db.display()
                ))));
            }
            tx.commit()?;
            Ok(())
        })();
        self.conn
            .execute_batch("DETACH DATABASE old; PRAGMA foreign_keys = ON")?;
        copied?;
        for sub in ["runs", "artifacts", "experiments"] {
            copy_tree(&old_dir.join(sub), &self.dir.join(sub))?;
            // Copied: the workspace keeps nothing (ADR 0014, 0016).
            let _ = std::fs::remove_dir_all(old_dir.join(sub));
        }
        crate::output::sweep(self)?;
        for f in ["prova.db", "prova.db-wal", "prova.db-shm"] {
            let p = old_dir.join(f);
            if p.exists() {
                std::fs::rename(&p, old_dir.join(format!("{f}.imported")))?;
            }
        }
        std::fs::write(
            old_dir.join("MOVED"),
            format!(
                "prova history for this workspace now lives in {}\n",
                self.dir.display()
            ),
        )?;
        Ok(())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn repo(&self) -> Option<&crate::home::Repo> {
        self.repo.as_ref()
    }

    /// True when the central home was unusable and data stays in the workspace.
    pub fn is_fallback(&self) -> bool {
        self.fallback
    }

    fn migrate(&mut self) -> Result<()> {
        let current: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let latest = MIGRATIONS.len() as i64;
        if current > latest {
            return Err(StoreError::TooNew(current, latest));
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            let tx = self
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            // Another process may have migrated while we waited for the lock.
            let now: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            if now > i as i64 {
                continue;
            }
            tx.execute_batch(sql)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
            tx.commit()?;
        }
        // Distilled output (ADR 0016): files left by earlier versions are
        // distilled into the store, then removed.
        if current < 5 && latest >= 5 {
            crate::output::sweep(self)?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn run_dir(&self, run: &str) -> PathBuf {
        self.dir.join("runs").join(run)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn create_run(&self, r: &NewRun<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO runs (id, suite, spec, cwd, code, env, machine, status, created_ms, origin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'created', ?8, ?9)",
            params![
                r.id,
                r.suite,
                r.spec.to_string(),
                r.cwd,
                r.code,
                r.env.to_string(),
                r.machine.to_string(),
                r.created_ms as i64,
                r.origin.to_string()
            ],
        )?;
        Ok(())
    }

    /// created -> running. Returns false when the run was cancelled first.
    pub fn mark_started(&self, run: &str, supervisor_pid: u32, now_ms: u64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE runs SET status = 'running', started_ms = ?2, supervisor_pid = ?3
             WHERE id = ?1 AND status = 'created'",
            params![run, now_ms as i64, supervisor_pid],
        )?;
        Ok(n == 1)
    }

    /// created -> cancelled, atomically. Returns false if it already started.
    pub fn cancel_created(&self, run: &str, now_ms: u64) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let status = run_status(&tx, run)?;
        if status != Status::Created {
            return Ok(false);
        }
        insert_event(
            &tx,
            run,
            1,
            now_ms,
            &EventKind::RunFinished {
                status: Status::Cancelled,
                exit_code: None,
                reason: Some("cancelled before start".into()),
                leaked: vec![],
            },
        )?;
        tx.execute(
            "UPDATE runs SET status = 'cancelled', ended_ms = ?2, reason = 'cancelled before start'
             WHERE id = ?1",
            params![run, now_ms as i64],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn request_cancel(&self, run: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET cancel_requested = 1 WHERE id = ?1",
            params![run],
        )?;
        Ok(())
    }

    pub fn cancel_requested(&self, run: &str) -> Result<bool> {
        let v: i64 = self
            .conn
            .query_row(
                "SELECT cancel_requested FROM runs WHERE id = ?1",
                params![run],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::RunNotFound(run.into()))?;
        Ok(v != 0)
    }

    pub fn set_level(&self, run: &str, level: Level) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET level = MAX(level, ?2) WHERE id = ?1",
            params![run, level.as_u8()],
        )?;
        Ok(())
    }

    pub fn append_events(&self, events: &[Event]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut suite: Option<String> = None;
        for e in events {
            insert_event(&tx, &e.run, e.seq, e.ts_ms, &e.kind)?;
            if let EventKind::TestSize {
                test,
                attempt,
                size,
                evidence,
            } = &e.kind
            {
                if suite.is_none() {
                    suite = Some(tx.query_row(
                        "SELECT suite FROM runs WHERE id = ?1",
                        params![e.run],
                        |r| r.get(0),
                    )?);
                }
                let id = test_id(suite.as_deref().unwrap_or_default(), test);
                tx.execute(
                    "INSERT OR IGNORE INTO test_sizes (run_id, test_id, attempt, size, evidence)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![e.run, id, attempt, size.as_str(), evidence],
                )?;
            }
            if let EventKind::TestFinished {
                test,
                attempt,
                outcome,
                duration_ms,
                message,
                source,
                // Subtests have a history of their own too.
                ..
            } = &e.kind
            {
                if suite.is_none() {
                    suite = Some(tx.query_row(
                        "SELECT suite FROM runs WHERE id = ?1",
                        params![e.run],
                        |r| r.get(0),
                    )?);
                }
                let suite = suite.as_deref().unwrap_or_default();
                let id = test_id(suite, test);
                tx.execute(
                    "INSERT OR IGNORE INTO tests (id, suite, file, path, name, display, first_seen_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![id, suite, test.file, serde_json::to_string(&test.path)?, test.name, test.display(), e.ts_ms as i64],
                )?;
                let fingerprint = outcome.is_failure().then(|| {
                    let lines: Vec<String> = message
                        .as_deref()
                        .unwrap_or("")
                        .lines()
                        .take(5)
                        .map(str::to_owned)
                        .collect();
                    crate::identity::output_fingerprint(&lines)
                });
                tx.execute(
                    "INSERT OR IGNORE INTO attempts
                       (run_id, test_id, attempt, outcome, duration_ms, message, fingerprint, source, ts_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        e.run, id, attempt, outcome.as_str(), duration_ms.map(|d| d as i64),
                        message, fingerprint, source.as_u8(), e.ts_ms as i64
                    ],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Records a result file copied to `.prova/artifacts/<sha256>`.
    pub fn add_artifact(&self, run: &str, kind: &str, source: &str, sha256: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO artifacts (run_id, kind, source, sha256) VALUES (?1, ?2, ?3, ?4)",
            params![run, kind, source, sha256],
        )?;
        Ok(())
    }

    /// Appends the final event and ends the run in one transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_run(
        &self,
        run: &str,
        seq: u64,
        now_ms: u64,
        status: Status,
        outcome: Option<Outcome>,
        exit_code: Option<i32>,
        reason: Option<&str>,
        leaked: Vec<u32>,
        fingerprint: Option<&str>,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        insert_event(
            &tx,
            run,
            seq,
            now_ms,
            &EventKind::RunFinished {
                status,
                exit_code,
                reason: reason.map(str::to_owned),
                leaked,
            },
        )?;
        tx.execute(
            "UPDATE runs SET status = ?2, outcome = ?3, ended_ms = ?4, exit_code = ?5,
                             reason = ?6, fingerprint = ?7
             WHERE id = ?1",
            params![
                run,
                status.as_str(),
                outcome.map(Outcome::as_str),
                now_ms as i64,
                exit_code,
                reason,
                fingerprint
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Marks a run whose supervisor disappeared as aborted, but only if it
    /// is still in `expected` state and its supervisor pid is unchanged
    /// (no race with a supervisor that just started or finished).
    pub fn abort_orphan(
        &self,
        run: &str,
        expected: Status,
        supervisor_pid: Option<u32>,
        now_ms: u64,
        reason: &str,
    ) -> Result<bool> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let current: Option<(String, Option<i64>)> = tx
            .query_row(
                "SELECT status, supervisor_pid FROM runs WHERE id = ?1",
                params![run],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let still = current
            .is_some_and(|(s, p)| s == expected.as_str() && p.map(|v| v as u32) == supervisor_pid);
        if !still {
            return Ok(false);
        }
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE run_id = ?1",
            params![run],
            |r| r.get(0),
        )?;
        insert_event(
            &tx,
            run,
            seq as u64,
            now_ms,
            &EventKind::RunFinished {
                status: Status::Aborted,
                exit_code: None,
                reason: Some(reason.to_owned()),
                leaked: vec![],
            },
        )?;
        tx.execute(
            "UPDATE runs SET status = 'aborted', outcome = 'errored', ended_ms = ?2, reason = ?3
             WHERE id = ?1",
            params![run, now_ms as i64, reason],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn run(&self, run: &str) -> Result<RunRow> {
        self.conn
            .query_row(
                &format!("{RUN_SELECT} WHERE id = ?1"),
                params![run],
                row_to_run,
            )
            .optional()?
            .ok_or_else(|| StoreError::RunNotFound(run.into()))
    }

    /// Resolves `last` and unique id prefixes.
    pub fn resolve_run(&self, query: &str) -> Result<String> {
        let id: Option<String> = if query == "last" {
            self.conn
                .query_row(
                    "SELECT id FROM runs ORDER BY created_ms DESC, id DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()?
        } else {
            if query.is_empty() || !query.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(StoreError::RunNotFound(query.into()));
            }
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM runs WHERE substr(id, 1, length(?1)) = ?1 LIMIT 2")?;
            let ids: Vec<String> = stmt
                .query_map(params![query], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            if ids.len() == 1 {
                ids.into_iter().next()
            } else {
                None
            }
        };
        id.ok_or_else(|| StoreError::RunNotFound(query.into()))
    }

    pub fn recent_runs(&self, limit: usize) -> Result<Vec<RunRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "{RUN_SELECT} ORDER BY created_ms DESC, id DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map(params![limit as i64], row_to_run)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Previous terminal runs of the same suite, newest first.
    /// Earlier ended runs of the same suite and the same selection (a run
    /// of one selected test says nothing about the whole suite).
    pub fn suite_history(
        &self,
        suite: &str,
        selection: Option<&str>,
        before_ms: u64,
        limit: usize,
    ) -> Result<Vec<RunRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "{RUN_SELECT} WHERE suite = ?1 AND created_ms < ?2 AND ended_ms IS NOT NULL
               AND json_extract(spec, '$.selection') IS ?4
             ORDER BY created_ms DESC LIMIT ?3"
        ))?;
        let rows = stmt.query_map(
            params![suite, before_ms as i64, limit as i64, selection],
            row_to_run,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Takes a run out of every test's history, with a reason.
    pub fn forget(&self, run: &str, reason: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO forgotten (run_id, reason, created_ms) VALUES (?1, ?2, ?3)",
            params![run, reason, crate::ids::now_ms() as i64],
        )?;
        Ok(())
    }

    /// Which of these runs passed with warnings (unverified, ADR 0017).
    pub fn unverified(&self, runs: &[String]) -> Result<std::collections::BTreeSet<String>> {
        let mut out = std::collections::BTreeSet::new();
        let mut stmt = self
            .conn
            .prepare("SELECT 1 FROM events WHERE run_id = ?1 AND kind = 'run_warning' LIMIT 1")?;
        for r in runs {
            if stmt.exists(rusqlite::params![r])? {
                out.insert(r.clone());
            }
        }
        Ok(out)
    }

    pub fn events_after(&self, run: &str, after: u64) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, ts_ms, payload FROM events WHERE run_id = ?1 AND seq > ?2 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![run, after as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, ts, payload) = row?;
            out.push(Event {
                v: crate::model::EVENT_VERSION,
                run: run.to_owned(),
                seq: seq as u64,
                ts_ms: ts as u64,
                kind: serde_json::from_str(&payload)?,
            });
        }
        Ok(out)
    }

    pub fn last_seq(&self, run: &str) -> Result<u64> {
        let v: Option<i64> = self.conn.query_row(
            "SELECT MAX(seq) FROM events WHERE run_id = ?1",
            params![run],
            |r| r.get(0),
        )?;
        Ok(v.unwrap_or(0) as u64)
    }

    /// Changes whenever another connection commits; cheap to poll.
    pub fn data_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))?)
    }
}

/// Copies a directory tree, keeping files that already exist.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    let Ok(entries) = std::fs::read_dir(from) else {
        return Ok(());
    };
    private_dir(to)?;
    for e in entries.flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if e.file_type()?.is_dir() {
            copy_tree(&src, &dst)?;
        } else if !dst.exists() {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// Creates a directory (and parents) readable by the owner only.
pub fn private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

/// Creates or truncates a file readable by the owner only.
pub fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

/// Stable test identity (ADR 0006): suite, file, suite path and name.
pub fn test_id(suite: &str, t: &crate::model::TestKey) -> String {
    let key = format!(
        "{suite}\0{}\0{}\0{}",
        t.file.as_deref().unwrap_or(""),
        t.path.join("\u{1f}"),
        t.name
    );
    crate::ids::sha256_hex(key.as_bytes())[..16].to_owned()
}

fn run_status(conn: &Connection, run: &str) -> Result<Status> {
    let s: String = conn
        .query_row("SELECT status FROM runs WHERE id = ?1", params![run], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| StoreError::RunNotFound(run.into()))?;
    Ok(Status::parse(&s).unwrap_or(Status::Aborted))
}

fn insert_event(conn: &Connection, run: &str, seq: u64, ts: u64, kind: &EventKind) -> Result<()> {
    conn.execute(
        "INSERT INTO events (run_id, seq, ts_ms, kind, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            run,
            seq as i64,
            ts as i64,
            kind.name(),
            serde_json::to_string(kind)?
        ],
    )?;
    Ok(())
}

const RUN_SELECT: &str = "SELECT id, suite, spec, cwd, code, status, outcome, level, created_ms,
    started_ms, ended_ms, exit_code, reason, supervisor_pid, cancel_requested, fingerprint, origin FROM runs";

fn row_to_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunRow> {
    let spec: String = r.get(2)?;
    let status: String = r.get(5)?;
    let outcome: Option<String> = r.get(6)?;
    Ok(RunRow {
        id: r.get(0)?,
        suite: r.get(1)?,
        spec: serde_json::from_str(&spec).unwrap_or(serde_json::Value::Null),
        cwd: r.get(3)?,
        code: r.get(4)?,
        status: Status::parse(&status).unwrap_or(Status::Aborted),
        outcome: outcome.as_deref().and_then(Outcome::parse),
        level: Level::from_u8(r.get::<_, i64>(7)? as u8),
        created_ms: r.get::<_, i64>(8)? as u64,
        started_ms: r.get::<_, Option<i64>>(9)?.map(|v| v as u64),
        ended_ms: r.get::<_, Option<i64>>(10)?.map(|v| v as u64),
        exit_code: r.get(11)?,
        reason: r.get(12)?,
        supervisor_pid: r.get::<_, Option<i64>>(13)?.map(|v| v as u32),
        cancel_requested: r.get::<_, i64>(14)? != 0,
        fingerprint: r.get(15)?,
        origin: serde_json::from_str(&r.get::<_, String>(16)?).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EVENT_VERSION, TestKey};

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "prova-store-{}-{}",
            std::process::id(),
            crate::ids::new_run_id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn new_run(s: &Store, id: &str) {
        let v = serde_json::json!({});
        s.create_run(&NewRun {
            id,
            suite: "s",
            spec: &v,
            cwd: "/",
            code: "nogit",
            env: &v,
            machine: &v,
            origin: &v,
            created_ms: 1,
        })
        .unwrap();
    }

    fn ev(run: &str, seq: u64, kind: EventKind) -> Event {
        Event {
            v: EVENT_VERSION,
            run: run.into(),
            seq,
            ts_ms: 1,
            kind,
        }
    }

    #[test]
    fn events_after_end_are_refused_by_the_database() {
        let d = tmp();
        let s = Store::open_dir(&d, &d.join(".prova")).unwrap();
        new_run(&s, "r1");
        assert!(s.mark_started("r1", 1, 2).unwrap());
        s.append_events(&[ev("r1", 1, EventKind::RunStarted { pid: 3 })])
            .unwrap();
        s.finish_run(
            "r1",
            2,
            3,
            Status::Cancelled,
            None,
            None,
            None,
            vec![],
            None,
        )
        .unwrap();
        let late = ev(
            "r1",
            3,
            EventKind::TestStarted {
                test: TestKey::new(vec![], "a"),
            },
        );
        assert!(s.append_events(&[late]).is_err());
        assert_eq!(s.run("r1").unwrap().status, Status::Cancelled);
    }

    #[test]
    fn inputs_and_terminal_status_are_immutable() {
        let d = tmp();
        let s = Store::open_dir(&d, &d.join(".prova")).unwrap();
        new_run(&s, "r1");
        assert!(
            s.conn()
                .execute("UPDATE runs SET code = 'x' WHERE id = 'r1'", [])
                .is_err()
        );
        assert!(s.cancel_created("r1", 5).unwrap());
        assert!(
            !s.mark_started("r1", 1, 6).unwrap(),
            "cancelled run must not start"
        );
        assert!(
            s.conn()
                .execute(
                    "UPDATE runs SET status = 'running', ended_ms = NULL WHERE id = 'r1'",
                    []
                )
                .is_err()
        );
    }

    #[test]
    fn second_open_sees_first_writers_events() {
        let dir = tmp();
        let a = Store::open_dir(&dir, &dir.join(".prova")).unwrap();
        let b = Store::open_dir(&dir, &dir.join(".prova")).unwrap();
        new_run(&a, "r1");
        a.mark_started("r1", 1, 2).unwrap();
        let v0 = b.data_version().unwrap();
        a.append_events(&[ev("r1", 1, EventKind::RunStarted { pid: 3 })])
            .unwrap();
        assert_ne!(b.data_version().unwrap(), v0);
        assert_eq!(b.events_after("r1", 0).unwrap().len(), 1);
        assert_eq!(b.resolve_run("r").ok(), None, "ids are hex");
        assert_eq!(b.resolve_run("last").unwrap(), "r1");
    }
}
