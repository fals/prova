//! Medium: the code identity is the git tree id the working content would
//! have (ADR 0038 section 1). Needs real git; a smaller test cannot compare
//! with what git itself computes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use prova_core::identity::code_identity;

fn git(d: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(d)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn repo(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-content-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("src/deep")).unwrap();
    std::fs::write(d.join("src/a.txt"), "a\n").unwrap();
    std::fs::write(d.join("src/deep/b.txt"), "b\n").unwrap();
    std::fs::write(d.join("run.sh"), "#!/bin/sh\n").unwrap();
    std::fs::write(d.join(".gitignore"), "ignored.log\n").unwrap();
    git(&d, &["init", "-q"]);
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "one"]);
    d
}

fn tree12(d: &Path) -> String {
    format!("tree:{}", &git(d, &["rev-parse", "HEAD^{tree}"])[..12])
}

/// Every file under `.git/objects`: nothing may be added by computing an id.
fn objects(d: &Path) -> Vec<PathBuf> {
    fn walk(p: &Path, v: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(p).unwrap().flatten() {
            if e.path().is_dir() {
                walk(&e.path(), v);
            } else {
                v.push(e.path());
            }
        }
    }
    let mut v = vec![];
    walk(&d.join(".git/objects"), &mut v);
    v.sort();
    v
}

#[test]
fn content_id_edited_tree_has_the_id_the_next_commit_gets_and_nothing_is_written() {
    let d = repo("same");
    let clean = code_identity(&d);
    assert_eq!(clean, tree12(&d), "a clean checkout keeps HEAD's tree id");

    std::fs::write(d.join("src/deep/b.txt"), "b2\n").unwrap();
    std::fs::write(d.join("new.txt"), "new\n").unwrap();
    std::fs::remove_file(d.join("src/a.txt")).unwrap();
    let before = objects(&d);
    let edited = code_identity(&d);
    assert_eq!(objects(&d), before, "computing the identity writes nothing");
    assert_ne!(edited, clean);
    assert!(!edited.contains("dirty"), "{edited}");
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "two"]);
    assert_eq!(edited, tree12(&d), "same content, same code");
}

#[test]
fn content_id_different_edits_differ_and_untracked_files_count() {
    let d = repo("differ");
    let clean = code_identity(&d);
    std::fs::write(d.join("src/a.txt"), "x\n").unwrap();
    let x = code_identity(&d);
    std::fs::write(d.join("src/a.txt"), "y\n").unwrap();
    assert_ne!(x, code_identity(&d));
    std::fs::write(d.join("src/a.txt"), "a\n").unwrap();
    assert_eq!(code_identity(&d), clean, "reverting restores the id");
    std::fs::write(d.join("u.txt"), "1\n").unwrap();
    let u1 = code_identity(&d);
    assert_ne!(u1, clean, "an untracked file is content");
    std::fs::write(d.join("u.txt"), "2\n").unwrap();
    assert_ne!(code_identity(&d), u1);
}

#[test]
fn content_id_ignored_files_and_caches_do_not_count() {
    let d = repo("ignored");
    let clean = code_identity(&d);
    std::fs::write(d.join("ignored.log"), "noise\n").unwrap();
    std::fs::create_dir_all(d.join("__pycache__")).unwrap();
    std::fs::write(d.join("__pycache__/x.pyc"), b"\0").unwrap();
    std::fs::create_dir_all(d.join("target/debug")).unwrap();
    std::fs::write(d.join("target/debug/x"), b"\0").unwrap();
    assert_eq!(code_identity(&d), clean);
}

#[cfg(unix)]
#[test]
fn content_id_executable_bit_and_symlinks_are_content() {
    use std::os::unix::fs::PermissionsExt;
    let d = repo("mode");
    let clean = code_identity(&d);
    let mut p = std::fs::metadata(d.join("run.sh")).unwrap().permissions();
    p.set_mode(0o755);
    std::fs::set_permissions(d.join("run.sh"), p).unwrap();
    let exec = code_identity(&d);
    assert_ne!(exec, clean, "the executable bit changes the id");
    std::os::unix::fs::symlink("src/a.txt", d.join("link")).unwrap();
    let with_link = code_identity(&d);
    assert_ne!(with_link, exec);
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "two"]);
    assert_eq!(
        with_link,
        tree12(&d),
        "mode and symlink match git's own tree"
    );
}
