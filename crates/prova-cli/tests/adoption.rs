//! Medium tests for adoption (ADR 0012, 0013): prova init, prova adopt.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-adopt-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn prova(dir: &Path, args: &[&str]) -> Output {
    Command::new(PROVA)
        .args(args)
        .current_dir(dir)
        .env("PROVA_HOME", home_of(dir))
        .output()
        .expect("run prova")
}

#[test]
fn init_installs_skills_and_is_idempotent() {
    let d = workspace("init");
    std::fs::write(d.join("AGENTS.md"), "# Project\n\nOwn rules.\n").unwrap();
    std::fs::write(d.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    std::fs::create_dir_all(d.join(".claude")).unwrap();
    assert!(prova(&d, &["init"]).status.success());
    assert!(prova(&d, &["init"]).status.success());
    for s in [
        "prova-adopt",
        "prova-test",
        "prova-experiment",
        "prova-rust",
    ] {
        assert!(
            d.join(".agents/skills").join(s).join("SKILL.md").exists(),
            "{s}"
        );
        assert!(
            d.join(".claude/skills").join(s).join("SKILL.md").exists(),
            "{s}"
        );
    }
    assert!(
        d.join(".agents/skills/prova-rust/references/pitfalls.md")
            .exists()
    );
    let agents = std::fs::read_to_string(d.join("AGENTS.md")).unwrap();
    assert!(
        agents.starts_with("# Project\n\nOwn rules."),
        "user content kept"
    );
    assert_eq!(
        agents.matches("<!-- prova:begin -->").count(),
        1,
        "section not duplicated"
    );
    let gi = std::fs::read_to_string(d.join(".gitignore")).unwrap();
    assert_eq!(gi.matches(".prova/").count(), 1);
}

#[test]
fn adopt_proposes_and_writes_without_overwriting() {
    let d = workspace("propose");
    std::fs::write(d.join("go.mod"), "module x\n").unwrap();
    std::fs::write(d.join("Makefile"), "test:\n\tgo test ./...\n").unwrap();
    let o = prova(&d, &["adopt", "--write"]);
    assert!(o.status.success());
    let t = std::fs::read_to_string(d.join("prova.toml")).unwrap();
    assert!(
        t.contains("run = \"make test\"") && t.contains("go test -json -count=1 -run {filter}"),
        "{t}"
    );
    assert_eq!(
        prova(&d, &["adopt", "--write"]).status.code(),
        Some(4),
        "never overwrites"
    );
}

#[test]
fn verify_detects_selective_and_unselective_templates() {
    let d = workspace("verify");
    std::fs::write(
        d.join("prova.toml"),
        r#"[suite.good]
run = "true"
select = "printf 'test m::%s ... ok\n' {filter}"

[suite.bad]
run = "true"
select = "printf 'test m::%s ... ok\ntest m::other ... ok\n' {filter}"

[suite.blind]
run = "true"
select = "echo {filter}"
"#,
    )
    .unwrap();
    let ok = prova(&d, &["adopt", "--verify", "good", "--filter", "parse"]);
    assert_eq!(
        ok.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ok.stdout)
    );
    let bad = prova(&d, &["adopt", "--verify", "bad", "--filter", "parse"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stdout).contains("not selective"));
    let blind = prova(&d, &["adopt", "--verify", "blind", "--filter", "parse"]);
    assert_eq!(blind.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&blind.stdout).contains("level opaque"));
}

/// Isolated prova home for a test workspace, outside it (ADR 0014).
fn home_of(dir: &Path) -> PathBuf {
    PathBuf::from(format!("{}.home", dir.display()))
}
