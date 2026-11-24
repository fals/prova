//! Medium: recording a test result looks up that test's history; the lookups
//! must never read every event of every earlier run (issue #91: 8 ms per
//! result once history existed, growing with it). Needs a store file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use prova_core::store::Store;

fn plan(s: &Store, sql: &str) -> String {
    let mut stmt = s
        .conn()
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(3))
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>()
        .join("\n")
}

fn store() -> Store {
    let d = std::env::temp_dir().join(format!("prova-plans-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    Store::open_dir(&d, &d.join("store")).unwrap()
}

#[test]
fn the_unverified_run_check_uses_an_index_on_warnings() {
    let s = store();
    let p = plan(
        &s,
        "SELECT 1 FROM runs r3 WHERE NOT EXISTS (SELECT 1 FROM events w WHERE w.run_id = r3.id \
         AND w.kind = 'run_warning' AND w.payload LIKE '%x%')",
    );
    assert!(p.contains("events_warnings"), "{p}");
}

#[test]
fn the_run_size_lookup_uses_an_index_on_run_sizes() {
    let s = store();
    let p = plan(
        &s,
        "SELECT json_extract(e.payload, '$.size') FROM events e WHERE e.kind = 'run_size' \
         AND e.run_id IN (SELECT run_id FROM attempts WHERE test_id = 'x')",
    );
    assert!(p.contains("events_run_sizes"), "{p}");
}
