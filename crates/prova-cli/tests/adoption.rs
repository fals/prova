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

/// Round 6: an anchored filter (`^TestAdd$`, as Go wants it) names the
/// test, not prova's full name; verify must accept it.
#[test]
fn verify_accepts_an_anchored_filter() {
    let d = std::env::temp_dir().join(format!("prova-verify-anchor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("prova.toml"),
        "[suite.t]\nrun = \"echo 'test pkg::a ... ok'; echo 'test pkg::b ... ok'\"\nselect = \"echo 'test pkg::b ... ok'\"\n",
    )
    .unwrap();
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_prova"))
        .args(["adopt", "--verify", "t", "--filter", "^b$"])
        .current_dir(&d)
        .env("PROVA_HOME", d.join("home"))
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.starts_with("VERIFIED"), "{out}");
}

/// Round 7: Vitest `-t` reports the tests it did not select as skipped;
/// they did not run, and selection works.
#[test]
fn verify_ignores_tests_the_filter_skipped() {
    let d = std::env::temp_dir().join(format!("prova-verify-skip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("prova.toml"),
        "[suite.t]\nrun = \"echo 'test pkg::a ... ok'; echo 'test pkg::b ... ok'\"\nselect = \"echo 'test pkg::a ... ignored'; echo 'test pkg::b ... ok'\"\n",
    )
    .unwrap();
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_prova"))
        .args(["adopt", "--verify", "t", "--filter", "b"])
        .current_dir(&d)
        .env("PROVA_HOME", d.join("home"))
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.starts_with("VERIFIED"), "{out}");
}

/// Every major harness finds prova (researched 2026-11-24): skills in
/// `.agents/skills` and `.claude/skills` always (Claude Code and Cline read
/// only the latter); the section in AGENTS.md, and in the instruction files
/// that already exist and would otherwise hide it (CLAUDE.md for Claude
/// Code, GEMINI.md, Zed's first-match files); a harness's own skills
/// directory only where it is already set up (Kiro, Junie). Nothing is
/// created that would shadow AGENTS.md.
#[test]
fn init_reaches_every_harness_without_shadowing_agents_md() {
    let d = workspace("harnesses");
    std::fs::write(d.join("CLAUDE.md"), "# Claude\n\nOwn rules.\n").unwrap();
    std::fs::write(d.join(".cursorrules"), "be brief\n").unwrap();
    std::fs::create_dir_all(d.join(".kiro")).unwrap();
    assert!(prova(&d, &["init"]).status.success());
    assert!(prova(&d, &["init"]).status.success());
    for dir in [".agents/skills", ".claude/skills", ".kiro/skills"] {
        assert!(d.join(dir).join("prova-test/SKILL.md").exists(), "{dir}");
    }
    assert!(!d.join(".junie").exists(), "no .junie/ without Junie");
    for f in ["AGENTS.md", "CLAUDE.md", ".cursorrules"] {
        let text = std::fs::read_to_string(d.join(f)).unwrap();
        assert_eq!(
            text.matches("<!-- prova:begin -->").count(),
            1,
            "{f}: {text}"
        );
    }
    assert!(
        std::fs::read_to_string(d.join("CLAUDE.md"))
            .unwrap()
            .starts_with("# Claude\n\nOwn rules."),
        "user content kept"
    );
    // Files that would hide AGENTS.md from a harness (Zed reads only the
    // first it finds) are never created.
    for f in ["GEMINI.md", ".github/copilot-instructions.md", ".rules"] {
        assert!(!d.join(f).exists(), "{f} created");
    }
    let agents = std::fs::read_to_string(d.join("AGENTS.md")).unwrap();
    assert!(
        agents.contains(".agents/skills"),
        "says where the skills are"
    );

    // A CLAUDE.md that imports AGENTS.md already gets prova through it.
    let e = workspace("imports");
    std::fs::write(e.join("CLAUDE.md"), "@AGENTS.md\n").unwrap();
    assert!(prova(&e, &["init"]).status.success());
    assert_eq!(
        std::fs::read_to_string(e.join("CLAUDE.md")).unwrap(),
        "@AGENTS.md\n"
    );
}
