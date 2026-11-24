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

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Marker damage must never lose user text: the file stays byte-identical,
/// init says which file and what is wrong, and repeating changes nothing.
/// (Medium: needs the real binary and files; a unit test cannot see exit codes.)
#[test]
fn init_leaves_broken_markers_untouched_and_names_them() {
    let cases = [
        (
            "stray-begin",
            "# top\n<!-- prova:begin -->\nold stuff\n## My important section\nuser text\n",
            "begin without end",
        ),
        (
            "reversed",
            "# top\n<!-- prova:end -->\nmid\n<!-- prova:begin -->\ntail\n",
            "end before begin",
        ),
        (
            "duplicate",
            "<!-- prova:begin -->\na\n<!-- prova:end -->\nuser\n<!-- prova:begin -->\nb\n<!-- prova:end -->\n",
            "duplicate",
        ),
    ];
    for (name, content, problem) in cases {
        let d = workspace(&format!("markers-{name}"));
        std::fs::write(d.join("AGENTS.md"), content).unwrap();
        for _ in 0..2 {
            let o = prova(&d, &["init"]);
            assert_eq!(o.status.code(), Some(4), "{name}: {}", stderr(&o));
            let err = stderr(&o);
            assert!(
                err.contains("AGENTS.md") && err.contains(problem),
                "{name}: {err}"
            );
            assert_eq!(
                std::fs::read_to_string(d.join("AGENTS.md")).unwrap(),
                content,
                "{name}: file must be untouched"
            );
        }
    }
}

#[test]
fn init_without_markers_appends_once_and_keeps_user_text() {
    let d = workspace("no-markers");
    std::fs::write(d.join("AGENTS.md"), "# top\nuser text").unwrap();
    assert!(prova(&d, &["init"]).status.success());
    let first = std::fs::read_to_string(d.join("AGENTS.md")).unwrap();
    assert!(first.starts_with("# top\nuser text\n"));
    assert!(prova(&d, &["init"]).status.success());
    assert_eq!(std::fs::read_to_string(d.join("AGENTS.md")).unwrap(), first);
}

/// One bad target must not stop the others, and is named.
#[test]
fn init_reports_each_failing_file_and_does_the_rest() {
    let d = workspace("partial");
    std::fs::create_dir_all(d.join("AGENTS.md")).unwrap(); // a directory
    std::fs::write(d.join("CLAUDE.md"), "# claude\n").unwrap();
    let o = prova(&d, &["init"]);
    assert_eq!(o.status.code(), Some(4), "{}", stderr(&o));
    assert!(stderr(&o).contains("AGENTS.md"), "{}", stderr(&o));
    let claude = std::fs::read_to_string(d.join("CLAUDE.md")).unwrap();
    assert!(
        claude.contains("<!-- prova:begin -->"),
        "CLAUDE.md still done"
    );
    assert!(d.join(".agents/skills/prova-test/SKILL.md").exists());
    assert!(d.join(".gitignore").exists());
}

#[cfg(unix)]
#[test]
fn init_never_writes_through_a_symlink_leaving_the_repo() {
    let d = workspace("symlink");
    let outside = workspace("symlink-outside");
    let target = outside.join("shared.md");
    std::fs::write(&target, "# shared, not ours\n").unwrap();
    std::os::unix::fs::symlink(&target, d.join("AGENTS.md")).unwrap();
    std::fs::write(d.join("CLAUDE.md"), "# claude\n").unwrap();
    let o = prova(&d, &["init"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "# shared, not ours\n"
    );
    let err = stderr(&o);
    assert!(
        err.contains("AGENTS.md") && err.contains("outside"),
        "{err}"
    );
    assert!(
        std::fs::read_to_string(d.join("CLAUDE.md"))
            .unwrap()
            .contains("prova:begin")
    );
    // A symlink that stays inside the repository is followed.
    let d2 = workspace("symlink-inside");
    std::fs::write(d2.join("real.md"), "# inside\n").unwrap();
    std::os::unix::fs::symlink(d2.join("real.md"), d2.join("AGENTS.md")).unwrap();
    assert!(prova(&d2, &["init"]).status.success());
    assert!(
        std::fs::read_to_string(d2.join("real.md"))
            .unwrap()
            .contains("prova:begin")
    );
}

#[test]
fn init_keeps_crlf_line_endings() {
    let d = workspace("crlf");
    std::fs::write(d.join("AGENTS.md"), "# top\r\nlast user line\r\n").unwrap();
    assert!(prova(&d, &["init"]).status.success());
    let first = std::fs::read_to_string(d.join("AGENTS.md")).unwrap();
    assert!(
        first.starts_with("# top\r\nlast user line\r\n"),
        "{first:?}"
    );
    assert!(
        !first.replace("\r\n", "").contains('\n'),
        "all CRLF: {first:?}"
    );
    assert!(first.contains("<!-- prova:begin -->\r\n"));
    assert!(prova(&d, &["init"]).status.success());
    assert_eq!(std::fs::read_to_string(d.join("AGENTS.md")).unwrap(), first);
}

#[test]
fn init_section_names_the_prova_home() {
    let d = workspace("home-text");
    assert!(prova(&d, &["init"]).status.success());
    let agents = std::fs::read_to_string(d.join("AGENTS.md")).unwrap();
    assert!(agents.contains("`PROVA_HOME`"), "{agents}");
}

/// Round 13 (Go): `go test -run TestX/case` runs the parent `TestX` too;
/// verify called a correct subtest template "not selective".
#[test]
fn verify_accepts_the_parent_of_a_selected_subtest() {
    let d = workspace("verify-subtest");
    std::fs::write(
        d.join("prova.toml"),
        "[suite.t]\nrun = \"i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; printf '=== RUN   TestB\\\\n--- PASS: TestB (0.00s)\\\\n=== RUN   TestX\\\\n=== RUN   TestX/case\\\\n    --- PASS: TestX/case (0.00s)\\\\n--- PASS: TestX (0.00s)\\\\n'\"\nselect = \"i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; printf '=== RUN   TestX\\\\n=== RUN   TestX/case\\\\n    --- PASS: TestX/case (0.00s)\\\\n--- PASS: TestX (0.00s)\\\\n'\"\n",
    )
    .unwrap();
    let o = prova(&d, &["adopt", "--verify", "t", "--filter", "TestX/case"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.starts_with("VERIFIED"), "{out}");
}

/// Round 14: `init` said it "updated" an AGENTS.md it had just created.
#[test]
fn init_says_created_for_a_new_agents_md() {
    let d = workspace("created");
    let o = prova(&d, &["init"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("created AGENTS.md with a prova section"),
        "{out}"
    );
    let o = prova(&d, &["init"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(!out.contains("created AGENTS.md"), "{out}");
}

/// Round 15: after `adopt --write`, the next step said `--filter <test
/// name>` though prova had seen the project's tests.
#[test]
fn adopt_write_suggests_a_test_it_has_seen() {
    let d = workspace("example");
    std::fs::write(d.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    prova(
        &d,
        &[
            "run",
            "--",
            "sh",
            "-c",
            "i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; echo 'test calc::adds ... ok'",
        ],
    );
    let o = prova(&d, &["adopt", "--write"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("--filter calc::adds"), "{out}");
}
