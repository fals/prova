//! SQLite store (ADR 0008). One file at `<root>/.toe/toe.db`.
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
    #[error("database schema version {0} is newer than this toe supports ({1})")]
    TooNew(i64, i64),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Ordered migrations; index + 1 is the `user_version` after applying.
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_runs.sql"),
    include_str!("migrations/0002_tests.sql"),
    include_str!("migrations/0003_experiments.sql"),
];

pub struct Store {
    conn: Connection,
    root: PathBuf,
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
    pub created_ms: u64,
}

impl Store {
    /// Opens (creating if needed) the store under `root/.toe`.
    pub fn open(root: &Path) -> Result<Store> {
        let dir = root.join(".toe");
        private_dir(&dir)?;
        let db = dir.join("toe.db");
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
        };
        store.migrate()?;
        Ok(store)
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
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn run_dir(&self, run: &str) -> PathBuf {
        self.root.join(".toe").join("runs").join(run)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn create_run(&self, r: &NewRun<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO runs (id, suite, spec, cwd, code, env, machine, status, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'created', ?8)",
            params![
                r.id,
                r.suite,
                r.spec.to_string(),
                r.cwd,
                r.code,
                r.env.to_string(),
                r.machine.to_string(),
                r.created_ms as i64
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
            if let EventKind::TestFinished {
                test,
                attempt,
                outcome,
                duration_ms,
                message,
                source,
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

    /// Records a result file copied to `.toe/artifacts/<sha256>`.
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
    pub fn suite_history(&self, suite: &str, before_ms: u64, limit: usize) -> Result<Vec<RunRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "{RUN_SELECT} WHERE suite = ?1 AND created_ms < ?2 AND ended_ms IS NOT NULL
             ORDER BY created_ms DESC LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![suite, before_ms as i64, limit as i64], row_to_run)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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
    started_ms, ended_ms, exit_code, reason, supervisor_pid, cancel_requested, fingerprint FROM runs";

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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EVENT_VERSION, TestKey};

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "toe-store-{}-{}",
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
        let s = Store::open(&tmp()).unwrap();
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
        let s = Store::open(&tmp()).unwrap();
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
        let a = Store::open(&dir).unwrap();
        let b = Store::open(&dir).unwrap();
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
