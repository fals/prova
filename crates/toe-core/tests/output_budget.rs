//! Medium: kept output stays within its budget (ADR 0016); a real SQLite
//! file is needed to measure what is stored.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;
use toe_core::output::{self, Distiller, Kept};
use toe_core::store::{NewRun, Store};

fn store(name: &str) -> (std::path::PathBuf, Store) {
    let d = std::env::temp_dir().join(format!("toe-budget-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let s = Store::open_dir(&d, &d.join("store")).unwrap();
    (d, s)
}

fn add_run(s: &Store, id: &str, created_ms: u64) {
    let v = json!({});
    s.create_run(&NewRun {
        id,
        suite: "t",
        spec: &json!({"command": {"argv": ["true"]}, "stall_after_ms": 1, "kill_after_ms": 1}),
        cwd: ".",
        code: "nogit",
        env: &v,
        machine: &v,
        origin: &v,
        created_ms,
    })
    .unwrap();
}

/// Output that does not compress: every line different.
fn noisy_excerpt(seed: u64) -> output::Excerpt {
    let mut d = Distiller::default();
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    for i in 0..4000 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        d.push(&format!("error {i} {x:016x}{:016x}", x.rotate_left(17)));
    }
    d.finish(false)
}

#[test]
fn oldest_output_is_dropped_first_and_results_stay() {
    let (_d, s) = store("drop");
    for (i, id) in ["r1", "r2", "r3"].iter().enumerate() {
        add_run(&s, id, i as u64);
        output::keep_run(&s, id, &noisy_excerpt(i as u64), None).unwrap();
    }
    let size = |s: &Store| -> i64 {
        s.conn()
            .query_row("SELECT SUM(LENGTH(excerpt)) FROM run_output", [], |r| {
                r.get(0)
            })
            .unwrap()
    };
    let total = size(&s);
    // Room for two excerpts: the oldest goes.
    output::enforce_budget(&s, total * 2 / 3 + 1).unwrap();
    assert!(matches!(
        output::run_excerpt(&s, "r1").unwrap(),
        Kept::Pruned
    ));
    assert!(matches!(
        output::run_excerpt(&s, "r2").unwrap(),
        Kept::Excerpt(_)
    ));
    assert!(matches!(
        output::run_excerpt(&s, "r3").unwrap(),
        Kept::Excerpt(_)
    ));
    assert_eq!(s.run("r1").unwrap().id, "r1", "the run itself is kept");
    assert!(size(&s) <= total * 2 / 3 + 1);
}

#[test]
fn sweep_distills_leftover_logs_and_removes_files() {
    let (_d, s) = store("sweep");
    add_run(&s, "old", 1);
    s.conn()
        .execute("UPDATE runs SET status = 'finished', outcome = 'failed', ended_ms = 2 WHERE id = 'old'", [])
        .unwrap();
    let dir = s.run_dir("old");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("output.log"), "compiling\nerror: boom\ndone\n").unwrap();
    std::fs::create_dir_all(s.dir().join("artifacts")).unwrap();
    output::sweep(&s).unwrap();
    assert!(!dir.exists() && !s.dir().join("artifacts").exists());
    match output::run_excerpt(&s, "old").unwrap() {
        Kept::Excerpt(e) => assert_eq!(e.render(), "compiling\nerror: boom\ndone\n"),
        _ => panic!("excerpt expected"),
    }
}
