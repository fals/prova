//! Medium: code identity ignores caches that running tests writes (usability
//! round 3: Python bytecode split "same code" history). Needs a real git repo.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

#[test]
fn bytecode_and_tool_caches_do_not_change_the_code_identity() {
    let d = std::env::temp_dir().join(format!("prova-ident-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("tests")).unwrap();
    std::fs::write(d.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .current_dir(&d)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "x"]);
    let clean = prova_core::identity::code_identity(&d);
    std::fs::create_dir_all(d.join("__pycache__")).unwrap();
    std::fs::write(d.join("__pycache__/calc.cpython-313.pyc"), b"\x00bytecode").unwrap();
    std::fs::create_dir_all(d.join("tests/__pycache__")).unwrap();
    std::fs::write(d.join("tests/__pycache__/t.pyc"), b"\x01").unwrap();
    std::fs::create_dir_all(d.join(".pytest_cache/v")).unwrap();
    std::fs::write(d.join(".pytest_cache/v/lastfailed"), "{}").unwrap();
    assert_eq!(
        prova_core::identity::code_identity(&d),
        clean,
        "caches are not code"
    );
    std::fs::write(d.join("new.py"), "x = 1\n").unwrap();
    assert_ne!(
        prova_core::identity::code_identity(&d),
        clean,
        "a new source file is"
    );
}
