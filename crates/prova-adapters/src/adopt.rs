//! Framework detection and `prova.toml` proposals (ADR 0013 section 5).
//! Knowledge of frameworks lives here, next to the recognizers.

use std::path::Path;

/// One proposed suite.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SuiteProposal {
    pub name: String,
    pub run: String,
    pub select: Option<String>,
    pub why: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Proposal {
    /// Languages detected, matching skill names `prova-<language>`.
    pub languages: Vec<String>,
    /// The project's own test command, if one was found.
    pub existing: Option<String>,
    pub suites: Vec<SuiteProposal>,
    pub notes: Vec<String>,
    /// pytest-reportlog is importable here: Python suites get the native
    /// stream (`inject = ["pytest-reportlog"]`).
    pub reportlog: bool,
}

/// nextest's libtest JSON: experimental, enabled by an environment variable,
/// with the format version pinned so a change upstream cannot silently
/// change what prova reads.
const NEXTEST_JSON: &str = "NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run --no-fail-fast --message-format libtest-json --message-format-version 0.1 --success-output final";
/// nextest does not run doctests (round 9: a failing doctest gave a clean
/// PASS while `make test` failed): a crate with a library runs them after.
const NEXTEST_JSON_DOC: &str = "NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run --no-fail-fast --message-format libtest-json --message-format-version 0.1 --success-output final; s=$?; cargo test --doc || exit 1; exit $s";
const NEXTEST_JSON_SELECT: &str = "NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run --no-fail-fast --message-format libtest-json --message-format-version 0.1 --success-output final {filter}";

/// `cargo nextest --version` output, if nextest is installed.
fn nextest_version(root: &Path) -> Option<String> {
    let o = std::process::Command::new("cargo")
        .args(["nextest", "--version"])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    o.status.success().then(|| {
        let text = String::from_utf8_lossy(&o.stdout);
        text.lines().next().unwrap_or("").trim().to_owned()
    })
}

/// Version check: libtest JSON output exists from cargo-nextest 0.9.66.
fn nextest_has_libtest_json(version: &str) -> bool {
    let Some(v) = version
        .split_whitespace()
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))
    else {
        return false;
    };
    let n: Vec<u64> = v
        .split(['.', '-', '+'])
        .take(3)
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    n.len() == 3 && (n[0], n[1], n[2]) >= (0, 9, 66)
}

struct Framework {
    language: &'static str,
    /// The framework's own command, as a `make test` recipe would call it
    /// (`cargo test`), whatever `run` became (e.g. nextest).
    base: &'static str,
    run: &'static str,
    select: &'static str,
    why: String,
}

fn read(root: &Path, f: &str) -> String {
    std::fs::read_to_string(root.join(f)).unwrap_or_default()
}

fn any_file_with_ext(root: &Path, ext: &str) -> bool {
    std::fs::read_dir(root)
        .map(|d| {
            d.flatten()
                .any(|e| e.path().extension().is_some_and(|x| x == ext))
        })
        .unwrap_or(false)
}

/// Installed tool versions `prova adopt` asks about (replaced in tests).
type Probe = fn(&Path) -> Option<String>;

fn detect(root: &Path, nextest: Probe) -> Vec<Framework> {
    let mut out = Vec::new();
    if root.join("Cargo.toml").exists() {
        let nextest = nextest(root);
        out.push(match nextest.as_deref().filter(|v| nextest_has_libtest_json(v)) {
            Some(v) => {
                let lib = root.join("src/lib.rs").exists();
                Framework {
                    language: "rust",
                    base: "cargo test",
                    run: if lib { NEXTEST_JSON_DOC } else { NEXTEST_JSON },
                    select: NEXTEST_JSON_SELECT,
                    why: format!(
                        "Cargo.toml and {v}; its libtest JSON stream is read live (native; experimental format pinned to 0.1){}",
                        if lib { "; nextest skips doctests, so `cargo test --doc` runs them after" } else { "" }
                    ),
                }
            }
            None => Framework {
                language: "rust",
            base: "cargo test",
                run: "cargo test",
                select: "cargo test -- {filter}",
                why: "Cargo.toml; libtest output is recognized live (cargo-nextest 0.9.66+ would give the native stream)".into(),
            },
        });
    }
    if root.join("go.mod").exists() {
        out.push(Framework {
            language: "go",
            base: "go test",
            run: "go test -json -count=1 ./...",
            select: "go test -json -count=1 -run {filter} ./...",
            why: "go.mod; -json gives the native event stream, -count=1 disables the test cache"
                .into(),
        });
    }
    let py = [
        "pytest.ini",
        "pyproject.toml",
        "setup.cfg",
        "tox.ini",
        "conftest.py",
    ]
    .iter()
    .any(|f| root.join(f).exists())
        || root.join("tests").join("conftest.py").exists();
    if py {
        out.push(Framework {
            language: "python",
            base: "pytest",
            run: "python3 -m pytest -v -rfE",
            select: "python3 -m pytest -v -rfE -k {filter}",
            why: "pytest config; -v lines are recognized live".into(),
        });
    }
    let pkg = read(root, "package.json");
    if pkg.contains("\"vitest\"") {
        out.push(Framework {
            language: "ts",
            base: "vitest",
            run: "npx vitest run --reporter=default --reporter=\"$PROVA_VITEST_REPORTER\"",
            select: "npx vitest run --reporter=default --reporter=\"$PROVA_VITEST_REPORTER\" -t {filter}",
            why: "vitest in package.json; prova's reporter streams every test live (native)".into(),
        });
    } else if pkg.contains("\"jest\"") {
        out.push(Framework {
            language: "ts",
            base: "jest",
            run: "npx jest --ci",
            select: "npx jest --ci -t {filter}",
            why: "jest in package.json; add jest-junit for per-test results".into(),
        });
    }
    if root.join("pom.xml").exists() {
        out.push(Framework {
            language: "java",
            base: "mvn",
            run: "mvn -q test",
            select: "mvn -q test -Dtest={filter}",
            why: "pom.xml; surefire reports are ingested".into(),
        });
    } else if root.join("build.gradle").exists() || root.join("build.gradle.kts").exists() {
        out.push(Framework {
            language: "java",
            base: "gradle",
            run: "./gradlew test",
            select: "./gradlew test --tests {filter}",
            why: "Gradle; build/test-results are ingested".into(),
        });
    }
    if any_file_with_ext(root, "sln") || any_file_with_ext(root, "csproj") {
        out.push(Framework {
            language: "csharp",
            base: "dotnet test",
            run: "dotnet test --logger trx",
            select: "dotnet test --logger trx --filter FullyQualifiedName~{filter}",
            why: ".NET; TRX results are ingested".into(),
        });
    }
    out
}

/// Recipe lines of the Makefile `test` target.
fn make_test_recipe(root: &Path) -> Vec<String> {
    for mk in ["Makefile", "makefile", "GNUmakefile"] {
        let text = read(root, mk);
        let mut lines = text.lines();
        if lines
            .by_ref()
            .any(|l| l.starts_with("test:") || l.starts_with("test :"))
        {
            return lines
                .take_while(|l| l.starts_with('\t'))
                .map(|l| l.trim().trim_start_matches('@').to_owned())
                .filter(|l| !l.is_empty())
                .collect();
        }
    }
    Vec::new()
}

/// The project's own entry point for tests, if any.
fn existing_command(root: &Path) -> Option<String> {
    for mk in ["Makefile", "makefile", "GNUmakefile"] {
        let text = read(root, mk);
        if text
            .lines()
            .any(|l| l.starts_with("test:") || l.starts_with("test :"))
        {
            return Some("make test".into());
        }
    }
    let pkg = read(root, "package.json");
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&pkg)
        && let Some(t) = v["scripts"]["test"].as_str()
        && !t.contains("no test specified")
    {
        return Some("npm test".into());
    }
    None
}

pub fn propose(root: &Path) -> Proposal {
    let mut p = propose_with(root, nextest_version);
    p.reportlog = p.languages.iter().any(|l| l == "python") && pytest_reportlog_installed(root);
    p
}

fn pytest_reportlog_installed(root: &Path) -> bool {
    std::process::Command::new("python3")
        .args(["-c", "import pytest_reportlog"])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn propose_with(root: &Path, nextest: Probe) -> Proposal {
    let frameworks = detect(root, nextest);
    let existing = existing_command(root);
    let mut p = Proposal {
        languages: frameworks.iter().map(|f| f.language.to_owned()).collect(),
        existing: existing.clone(),
        ..Proposal::default()
    };
    match (frameworks.as_slice(), &existing) {
        ([], None) => p
            .notes
            .push("no test framework or test command found; use `prova run -- <command>`".into()),
        ([], Some(cmd)) => {
            p.suites.push(SuiteProposal {
                name: "default".into(),
                run: cmd.clone(),
                select: None,
                why: "project's own test command; framework unknown, add `select` by hand".into(),
            });
        }
        ([f], _) => {
            // `make test` that only calls the framework: run the framework
            // directly at full detail (same tests), keep make as a suite.
            let recipe = make_test_recipe(root);
            // The recipe calls the framework itself (`cargo test --quiet`,
            // `python3 -m pytest -q`, `npx vitest run`, `./gradlew test`).
            let thin = existing.as_deref() == Some("make test")
                && recipe.len() == 1
                && recipe[0].contains(f.base);
            if thin {
                let quiet = recipe[0]
                    .split_whitespace()
                    .any(|w| w == "-q" || w == "--quiet");
                if quiet {
                    p.notes.push(format!(
                        "`make test` runs `{}` in quiet mode: passing tests are invisible there; the default suite runs the same tests verbosely",
                        recipe[0]
                    ));
                }
                p.suites.push(SuiteProposal {
                    name: "default".into(),
                    run: f.run.into(),
                    select: Some(f.select.into()),
                    why: format!(
                        "same tests as `make test` ({}), with per-test detail; {}",
                        recipe[0], f.why
                    ),
                });
                p.suites.push(SuiteProposal {
                    name: "make".into(),
                    run: "make test".into(),
                    select: None,
                    why: "the project's own command, exactly as CI runs it".into(),
                });
            } else {
                // The project's own command, plus prova's Vitest reporter when
                // it is an npm script running vitest (round 11: `npm test`
                // alone ran blind once the reporter was injected).
                let run = match &existing {
                    Some(cmd) if f.base == "vitest" && cmd.starts_with("npm ") => {
                        format!("{cmd} -- --reporter=default --reporter=\"$PROVA_VITEST_REPORTER\"")
                    }
                    Some(cmd) => cmd.clone(),
                    None => f.run.into(),
                };
                p.suites.push(SuiteProposal {
                    name: "default".into(),
                    run,
                    select: Some(f.select.into()),
                    why: match &existing {
                        Some(cmd) => {
                            format!("runs `{cmd}` as the project does; selection via {}", f.why)
                        }
                        None => f.why.clone(),
                    },
                });
            }
        }
        (many, _) => {
            if let Some(cmd) = &existing {
                p.suites.push(SuiteProposal {
                    name: "default".into(),
                    run: cmd.clone(),
                    select: None,
                    why: "project's own test command (several languages)".into(),
                });
            }
            for f in many {
                p.suites.push(SuiteProposal {
                    name: f.language.into(),
                    run: f.run.into(),
                    select: Some(f.select.into()),
                    why: f.why.clone(),
                });
            }
        }
    }
    p
}

pub fn to_toml(p: &Proposal) -> String {
    let mut s = String::from(
        "# Proposed by `prova adopt`. Verify selection with:\n#   prova adopt --verify <suite> --filter <test name>\n",
    );
    for n in &p.notes {
        s.push_str(&format!("# note: {n}\n"));
    }
    for suite in &p.suites {
        s.push_str(&format!(
            "\n# {}\n[suite.{}]\nrun = {:?}\n",
            suite.why, suite.name, suite.run
        ));
        if let Some(sel) = &suite.select {
            s.push_str(&format!("select = {sel:?}\n"));
        }
        let python = suite.run.contains("pytest")
            || (p.languages.iter().any(|l| l == "python")
                && p.existing.as_deref() == Some(suite.run.as_str()));
        // The Vitest reporter ships with prova: no install, so native it is.
        // Either command may load the reporter (round 9: `run = "npm test"`
        // with a reporter in `select` crashed without the injection).
        if suite.run.contains("PROVA_VITEST_REPORTER")
            || suite
                .select
                .as_deref()
                .is_some_and(|x| x.contains("PROVA_VITEST_REPORTER"))
        {
            s.push_str("inject = [\"vitest-stream\"]\n");
            continue;
        }
        if p.reportlog && python {
            s.push_str("# pytest-reportlog is installed: every test is read live (native)\ninject = [\"pytest-reportlog\"]\n");
            continue;
        }
        // The project's own command hides detail; offer the opt-in recipe.
        if p.existing.as_deref() == Some(suite.run.as_str()) {
            for (lang, recipe, what) in [
                (
                    "python",
                    "pytest-reportlog",
                    "every test live from pytest via PYTEST_ADDOPTS (needs pytest-reportlog)",
                ),
                (
                    "python",
                    "pytest-junit",
                    "per-test results from pytest via PYTEST_ADDOPTS",
                ),
                (
                    "go",
                    "go-nocache",
                    "stop go replaying cached passes via GOFLAGS",
                ),
            ] {
                if p.languages.iter().any(|l| l == lang) {
                    s.push_str(&format!("# inject = [\"{recipe}\"]  # opt-in: {what}\n"));
                }
            }
        }
        if suite.run.contains("vitest") {
            s.push_str(
                "# every test live (native): inject = [\"vitest-stream\"] and add\n#   --reporter=default --reporter=\"$PROVA_VITEST_REPORTER\" to run and select\n",
            );
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn propose(root: &Path) -> Proposal {
        propose_with(root, |_| None)
    }

    fn dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("prova-adopt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for (f, c) in files {
            if let Some(parent) = d.join(f).parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(d.join(f), c).unwrap();
        }
        d
    }

    #[test]
    fn go_project_with_makefile_keeps_make_test() {
        let p = propose(&dir(
            "go",
            &[
                ("go.mod", "module x"),
                ("Makefile", "test: vet\n\tgo vet ./...\n\tgo test ./...\n"),
            ],
        ));
        assert_eq!(p.languages, vec!["go"]);
        assert_eq!(p.suites[0].run, "make test");
        assert_eq!(
            p.suites[0].select.as_deref(),
            Some("go test -json -count=1 -run {filter} ./...")
        );
        let toml = to_toml(&p);
        let parsed: toml::Value = toml::from_str(&toml).unwrap();
        assert_eq!(
            parsed["suite"]["default"]["run"].as_str(),
            Some("make test")
        );
    }

    #[test]
    fn own_command_gets_an_opt_in_injection_hint() {
        let p = propose(&dir(
            "hint",
            &[
                ("pytest.ini", "[pytest]"),
                ("Makefile", "test: lint\n\truff .\n\tpytest -q\n"),
            ],
        ));
        let t = to_toml(&p);
        assert!(t.contains("# inject = [\"pytest-junit\"]"), "{t}");
        assert!(
            !t.contains("\ninject ="),
            "only a comment: injection stays opt-in"
        );
    }

    #[test]
    fn thin_make_wrapper_runs_the_framework_verbosely() {
        let p = propose(&dir(
            "thin",
            &[
                ("Cargo.toml", "[package]"),
                ("Makefile", "test:\n\tcargo test --quiet\n"),
            ],
        ));
        assert_eq!(p.suites[0].run, "cargo test");
        assert_eq!(p.suites[1].run, "make test");
        assert!(p.notes[0].contains("quiet"), "{:?}", p.notes);
    }

    /// Round 9: nextest skips doctests; a failing doctest gave a clean pass.
    #[test]
    fn a_rust_library_runs_its_doctests_after_nextest() {
        let d = dir(
            "doctests",
            &[
                ("Cargo.toml", "[package]\nname = \"x\""),
                ("src/lib.rs", ""),
            ],
        );
        let p = propose_with(&d, |_| Some("cargo-nextest 0.9.70".into()));
        assert_eq!(p.suites[0].run, NEXTEST_JSON_DOC);
        assert!(p.suites[0].why.contains("doctests"));
    }

    /// Round 9: a suite whose `select` loads prova's Vitest reporter must get
    /// the injection, even when `run` is the project's own `npm test`.
    #[test]
    fn a_vitest_select_with_the_reporter_gets_the_injection() {
        let t = to_toml(&propose(&dir(
            "vitest-select",
            &[(
                "package.json",
                r#"{"scripts":{"test":"vitest run"},"devDependencies":{"vitest":"3"}}"#,
            )],
        )));
        for block in t.split("[suite.").skip(1) {
            if block.contains("PROVA_VITEST_REPORTER")
                && !block
                    .lines()
                    .any(|l| l.starts_with('#') && l.contains("PROVA_VITEST_REPORTER"))
            {
                assert!(block.contains("\ninject = [\"vitest-stream\"]"), "{block}");
            }
        }
    }

    #[test]
    fn rust_gets_the_native_nextest_stream_when_nextest_is_new_enough() {
        let d = dir("nextest", &[("Cargo.toml", "[package]\nname = \"x\"")]);
        let p = propose_with(&d, |_| Some("cargo-nextest 0.9.70".into()));
        assert_eq!(p.suites[0].run, NEXTEST_JSON);
        assert!(
            p.suites[0]
                .select
                .as_deref()
                .unwrap()
                .ends_with(" {filter}")
        );
        let old = propose_with(&d, |_| Some("cargo-nextest 0.9.60".into()));
        assert_eq!(old.suites[0].run, "cargo test");
    }

    /// Regression (usability round 3): with nextest installed, a `make test`
    /// that only runs `cargo test --quiet` was no longer seen as a thin
    /// wrapper, and adopt proposed the quiet `make test` as the default.
    #[test]
    fn thin_make_wrapper_is_seen_through_when_nextest_is_installed() {
        let d = dir(
            "thin-nextest",
            &[
                ("Cargo.toml", "[package]\nname = \"x\""),
                ("Makefile", "test:\n\tcargo test --quiet\n"),
            ],
        );
        let p = propose_with(&d, |_| Some("cargo-nextest 0.9.70".into()));
        assert_eq!(p.suites[0].name, "default");
        assert_eq!(p.suites[0].run, NEXTEST_JSON);
        assert_eq!(p.suites[1].run, "make test");
    }

    #[test]
    fn notes_are_written_and_reportlog_is_used_when_installed() {
        let mut p = propose(&dir(
            "reportlog",
            &[
                ("pytest.ini", "[pytest]"),
                ("Makefile", "test:\n\tpython3 -m pytest -q\n"),
            ],
        ));
        p.reportlog = true;
        let t = to_toml(&p);
        assert!(t.contains("# note: `make test` runs"), "{t}");
        assert_eq!(
            t.matches("\ninject = [\"pytest-reportlog\"]").count(),
            2,
            "{t}"
        );
    }

    #[test]
    fn nextest_libtest_json_needs_0_9_66() {
        assert!(nextest_has_libtest_json(
            "cargo-nextest 0.9.66 (abc 2023-12-10)"
        ));
        assert!(nextest_has_libtest_json("cargo-nextest 0.9.104"));
        assert!(nextest_has_libtest_json("cargo-nextest 1.0.0-rc.1"));
        assert!(!nextest_has_libtest_json("cargo-nextest 0.9.65"));
        assert!(!nextest_has_libtest_json("cargo-nextest"));
    }

    #[test]
    fn vitest_suite_points_to_the_stream_recipe() {
        let t = to_toml(&propose(&dir(
            "vitest-hint",
            &[("package.json", r#"{"devDependencies":{"vitest":"3"}}"#)],
        )));
        assert!(t.contains("inject = [\"vitest-stream\"]"), "{t}");
        // Round 7: native by default, not a hint to edit by hand.
        assert!(
            t.contains("\ninject = [\"vitest-stream\"]") && t.contains("$PROVA_VITEST_REPORTER"),
            "{t}"
        );
    }

    #[test]
    fn unknown_framework_still_gets_a_suite() {
        let p = propose(&dir("make", &[("Makefile", "test:\n\t./run-tests.sh\n")]));
        assert_eq!(p.suites[0].run, "make test");
        assert!(p.suites[0].select.is_none());
        let none = propose(&dir("empty", &[]));
        assert!(none.suites.is_empty() && !none.notes.is_empty());
    }

    #[test]
    fn polyglot_gets_one_suite_per_language() {
        let p = propose(&dir(
            "poly",
            &[
                ("Cargo.toml", "[package]"),
                ("pytest.ini", "[pytest]"),
                (
                    "package.json",
                    r#"{"scripts":{"test":"vitest"},"devDependencies":{"vitest":"1"}}"#,
                ),
            ],
        ));
        let names: Vec<&str> = p.suites.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["default", "rust", "python", "ts"]);
    }
}
