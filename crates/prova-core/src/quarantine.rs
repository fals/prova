//! Quarantine: known-flaky tests set aside from the verdict, in the open
//! (ADR 0027). Stored per repository; never edits the user's project.

use std::collections::BTreeMap;

use rusqlite::params;
use serde::Serialize;

use crate::store::{Result, Store};

/// Default and maximum lifetime of one quarantine, in days.
pub const DEFAULT_DAYS: u32 = 14;
pub const MAX_DAYS: u32 = 90;
const DAY_MS: u64 = 86_400_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub test: String,
    pub reason: String,
    pub created_ms: u64,
    pub expires_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_ms: Option<u64>,
}

impl Entry {
    /// Applies to a run created at `ts`: added by then, not removed or
    /// expired by then.
    pub fn in_force_at(&self, ts: u64) -> bool {
        self.created_ms <= ts && self.expires_ms > ts && self.removed_ms.is_none_or(|r| r > ts)
    }
}

/// Quarantines a test for `days` (1 to `MAX_DAYS`). A quarantine already in
/// force for the test is replaced (renewed with the new reason).
pub fn add(store: &Store, test: &str, reason: &str, days: u32, now_ms: u64) -> Result<Entry> {
    let days = days.clamp(1, MAX_DAYS);
    store.conn().execute(
        "UPDATE quarantine SET removed_ms = ?2 WHERE test = ?1 AND removed_ms IS NULL",
        params![test, now_ms as i64],
    )?;
    let expires = now_ms + u64::from(days) * DAY_MS;
    store.conn().execute(
        "INSERT INTO quarantine (test, reason, created_ms, expires_ms) VALUES (?1, ?2, ?3, ?4)",
        params![test, reason, now_ms as i64, expires as i64],
    )?;
    Ok(Entry {
        test: test.to_owned(),
        reason: reason.to_owned(),
        created_ms: now_ms,
        expires_ms: expires,
        removed_ms: None,
    })
}

/// Removes the test's open quarantine; `false` when it had none.
pub fn remove(store: &Store, test: &str, now_ms: u64) -> Result<bool> {
    let n = store.conn().execute(
        "UPDATE quarantine SET removed_ms = ?2 WHERE test = ?1 AND removed_ms IS NULL",
        params![test, now_ms as i64],
    )?;
    Ok(n > 0)
}

/// Every quarantine, newest first; removed ones only with `include_removed`.
pub fn list(store: &Store, include_removed: bool) -> Result<Vec<Entry>> {
    let mut stmt = store.conn().prepare(
        "SELECT test, reason, created_ms, expires_ms, removed_ms FROM quarantine
          WHERE (?1 OR removed_ms IS NULL) ORDER BY created_ms DESC, id DESC",
    )?;
    let rows = stmt.query_map(params![include_removed], |r| {
        Ok(Entry {
            test: r.get(0)?,
            reason: r.get(1)?,
            created_ms: r.get::<_, i64>(2)? as u64,
            expires_ms: r.get::<_, i64>(3)? as u64,
            removed_ms: r.get::<_, Option<i64>>(4)?.map(|v| v as u64),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The quarantines that applied at `ts`, by test name.
pub fn in_force_at(store: &Store, ts: u64) -> Result<BTreeMap<String, Entry>> {
    Ok(list(store, true)?
        .into_iter()
        .filter(|e| e.in_force_at(ts))
        .map(|e| (e.test.clone(), e))
        .collect())
}

/// Quarantines of `test`s that had expired (not removed) by `ts` and were
/// the latest word on their test: reviewed by nobody since.
pub fn expired_at(store: &Store, ts: u64) -> Result<Vec<Entry>> {
    let all = list(store, true)?;
    let mut latest: BTreeMap<String, Entry> = BTreeMap::new();
    for e in all.into_iter().rev() {
        latest.insert(e.test.clone(), e); // oldest first: the newest wins
    }
    Ok(latest
        .into_values()
        .filter(|e| e.removed_ms.is_none() && e.created_ms <= ts && e.expires_ms <= ts)
        .collect())
}

/// The quarantine that sets `display` aside: its own, or its parent test's
/// (a Go subtest `TestX/case` follows `TestX`).
pub fn covers<'a>(q: &'a BTreeMap<String, Entry>, display: &str) -> Option<&'a Entry> {
    q.get(display).or_else(|| {
        display
            .match_indices('/')
            .find_map(|(i, _)| q.get(&display[..i]))
    })
}

/// Quarantines in force for the run (when it was created), by test name.
pub fn for_run(store: &Store, run: &str) -> Result<BTreeMap<String, Entry>> {
    let created = store.run(run)?.created_ms;
    in_force_at(store, created)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(created: u64, expires: u64, removed: Option<u64>) -> Entry {
        Entry {
            test: "t".into(),
            reason: "r".into(),
            created_ms: created,
            expires_ms: expires,
            removed_ms: removed,
        }
    }

    #[test]
    fn a_quarantine_applies_between_its_start_and_its_end() {
        let q = e(10, 20, None);
        assert!(!q.in_force_at(9));
        assert!(q.in_force_at(10) && q.in_force_at(19));
        assert!(!q.in_force_at(20), "expired quarantines no longer apply");
    }

    #[test]
    fn a_subtest_follows_its_parent() {
        let mut q = BTreeMap::new();
        q.insert("TestX".to_owned(), e(0, 10, None));
        assert!(covers(&q, "TestX").is_some());
        assert!(covers(&q, "TestX/case/deep").is_some());
        assert!(covers(&q, "TestXY").is_none());
    }

    #[test]
    fn a_removed_quarantine_still_explains_runs_made_before_its_removal() {
        let q = e(10, 100, Some(30));
        assert!(q.in_force_at(29));
        assert!(!q.in_force_at(30));
    }
}
