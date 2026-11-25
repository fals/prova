//! Quarantine: known-flaky tests marked in the open. A quarantine never
//! changes a verdict (ADR 0034): it names known failures.
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
    /// The test's file (binary, module) when its name alone is shared by
    /// tests of several: only that one is quarantined.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Set when the store knows the name in other places too: a result of
    /// unknown place (quiet `cargo test` names no binary) cannot be told
    /// from the other places' and is not covered by it.
    #[serde(skip)]
    pub ambiguous: bool,
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
pub fn add(
    store: &Store,
    test: &str,
    file: Option<&str>,
    reason: &str,
    days: u32,
    now_ms: u64,
) -> Result<Entry> {
    let days = days.clamp(1, MAX_DAYS);
    // One quarantine per name and place: another place's is not replaced.
    store.conn().execute(
        "UPDATE quarantine SET removed_ms = ?2 WHERE test = ?1 AND file IS ?3 AND removed_ms IS NULL",
        params![test, now_ms as i64, file],
    )?;
    let expires = now_ms + u64::from(days) * DAY_MS;
    store.conn().execute(
        "INSERT INTO quarantine (test, reason, created_ms, expires_ms, file) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![test, reason, now_ms as i64, expires as i64, file],
    )?;
    Ok(Entry {
        test: test.to_owned(),
        reason: reason.to_owned(),
        created_ms: now_ms,
        expires_ms: expires,
        removed_ms: None,
        file: file.map(str::to_owned),
        ambiguous: false,
    })
}

/// Removes the open quarantine of the test in `file` (`None`: the one made
/// without a place); `false` when it had none.
pub fn remove(store: &Store, test: &str, file: Option<&str>, now_ms: u64) -> Result<bool> {
    let n = store.conn().execute(
        "UPDATE quarantine SET removed_ms = ?2 WHERE test = ?1 AND file IS ?3 AND removed_ms IS NULL",
        params![test, now_ms as i64, file],
    )?;
    Ok(n > 0)
}

/// The key a quarantine is kept under: the name, and its place when it has
/// one (two crates' `tests::flaky` are two quarantines).
pub fn key_of(test: &str, file: Option<&str>) -> String {
    match file {
        Some(f) => format!("{f}\u{1f}{test}"),
        None => test.to_owned(),
    }
}

/// Every quarantine, newest first; removed ones only with `include_removed`.
pub fn list(store: &Store, include_removed: bool) -> Result<Vec<Entry>> {
    let mut stmt = store.conn().prepare(
        "SELECT test, reason, created_ms, expires_ms, removed_ms, file FROM quarantine
          WHERE (?1 OR removed_ms IS NULL) ORDER BY created_ms DESC, id DESC",
    )?;
    let rows = stmt.query_map(params![include_removed], |r| {
        Ok(Entry {
            test: r.get(0)?,
            reason: r.get(1)?,
            created_ms: r.get::<_, i64>(2)? as u64,
            expires_ms: r.get::<_, i64>(3)? as u64,
            removed_ms: r.get::<_, Option<i64>>(4)?.map(|v| v as u64),
            file: r.get(5)?,
            ambiguous: false,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The quarantines that applied at `ts`, by test name.
pub fn in_force_at(store: &Store, ts: u64) -> Result<BTreeMap<String, Entry>> {
    let mut out = BTreeMap::new();
    for mut e in list(store, true)?.into_iter().filter(|e| e.in_force_at(ts)) {
        if e.file.is_some() {
            e.ambiguous = files_of(store, &e.test)?.len() > 1;
        }
        out.insert(key_of(&e.test, e.file.as_deref()), e);
    }
    Ok(out)
}

/// Quarantines of `test`s that had expired (not removed) by `ts` and were
/// the latest word on their test: reviewed by nobody since.
pub fn expired_at(store: &Store, ts: u64) -> Result<Vec<Entry>> {
    let all = list(store, true)?;
    let mut latest: BTreeMap<String, Entry> = BTreeMap::new();
    for e in all.into_iter().rev() {
        latest.insert(key_of(&e.test, e.file.as_deref()), e); // oldest first: the newest wins
    }
    Ok(latest
        .into_values()
        .filter(|e| e.removed_ms.is_none() && e.created_ms <= ts && e.expires_ms <= ts)
        .collect())
}

/// The quarantine that covers `display`: its own, or its parent test's
/// (a Go subtest `TestX/case` follows `TestX`).
pub fn covers<'a>(q: &'a BTreeMap<String, Entry>, display: &str) -> Option<&'a Entry> {
    let of_name = |name: &str| q.values().find(|e| e.test == name);
    of_name(display).or_else(|| {
        display
            .match_indices('/')
            .find_map(|(i, _)| of_name(&display[..i]))
    })
}

/// Whether some quarantine (in any place) is of exactly this test name.
pub fn names_test(q: &BTreeMap<String, Entry>, display: &str) -> bool {
    q.values().any(|e| e.test == display)
}

/// The quarantine that covers the test with lifecycle key `unique`
/// (`file<US>display`): as `covers`, and of the same file when the
/// quarantine names one. A name can be quarantined in several places.
pub fn covers_key<'a>(q: &'a BTreeMap<String, Entry>, unique: &str) -> Option<&'a Entry> {
    let (file, display) = unique.rsplit_once('\u{1f}').unwrap_or(("", unique));
    // A test whose directory is unknown may be any directory's: no
    // quarantine covers it.
    if file == crate::model::UNKNOWN_PLACE {
        return None;
    }
    let of_place = |name: &str| {
        q.values().find(|e| {
            e.test == name
                && match (&e.file, file) {
                    (Some(f), k) if !k.is_empty() => f == k,
                    (Some(_), _) => !e.ambiguous,
                    _ => true,
                }
        })
    };
    of_place(display).or_else(|| {
        display
            .match_indices('/')
            .find_map(|(i, _)| of_place(&display[..i]))
    })
}

/// The files (binaries, modules) that hold a test of this name.
pub fn files_of(store: &Store, display: &str) -> Result<Vec<String>> {
    let mut stmt = store.conn().prepare(
        "SELECT DISTINCT file FROM tests WHERE display = ?1 AND file IS NOT NULL
            AND file NOT LIKE '(package pending)%' AND file NOT LIKE '(unknown directory)%'
          ORDER BY file",
    )?;
    Ok(stmt
        .query_map(params![display], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
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
            file: None,
            ambiguous: false,
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
        let mut x = e(0, 10, None);
        x.test = "TestX".into();
        q.insert(key_of("TestX", None), x);
        assert!(covers(&q, "TestX").is_some());
        assert!(covers(&q, "TestX/case/deep").is_some());
        assert!(covers(&q, "TestXY").is_none());
    }

    /// Round 17: under quiet `cargo test` no binary is named, and another
    /// crate's same-named failure was filed under calc's quarantine.
    #[test]
    fn a_result_of_unknown_place_is_not_set_aside_by_a_place_quarantine_of_a_shared_name() {
        let mut calc = e(0, 10, None);
        calc.file = Some("calc".into());
        let mut q = BTreeMap::new();
        q.insert("t".to_owned(), calc.clone());
        // The name is only calc's: a quiet run's failure is calc's.
        assert!(covers_key(&q, "\u{1f}t").is_some());
        assert!(covers_key(&q, "calc\u{1f}t").is_some());
        assert!(covers_key(&q, "geo\u{1f}t").is_none());
        // Another place has it too: unknown is not calc.
        calc.ambiguous = true;
        q.insert("t".to_owned(), calc);
        assert!(covers_key(&q, "\u{1f}t").is_none());
        assert!(covers_key(&q, "calc\u{1f}t").is_some());
    }

    /// Round 17: a name quarantined in two places covers each place's
    /// test, and no third.
    #[test]
    fn a_name_quarantined_in_two_places_covers_each_of_them() {
        let mut q = BTreeMap::new();
        for place in ["calc", "geo"] {
            let mut x = e(0, 10, None);
            x.file = Some(place.into());
            x.ambiguous = true;
            q.insert(key_of("t", Some(place)), x);
        }
        assert_eq!(q.len(), 2);
        assert!(covers_key(&q, "calc\u{1f}t").is_some_and(|e| e.file.as_deref() == Some("calc")));
        assert!(covers_key(&q, "geo\u{1f}t").is_some_and(|e| e.file.as_deref() == Some("geo")));
        assert!(covers_key(&q, "other\u{1f}t").is_none());
        assert!(covers_key(&q, "\u{1f}t").is_none(), "unknown place");
    }

    #[test]
    fn a_removed_quarantine_still_explains_runs_made_before_its_removal() {
        let q = e(10, 100, Some(30));
        assert!(q.in_force_at(29));
        assert!(!q.in_force_at(30));
    }
}
