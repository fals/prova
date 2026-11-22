//! Framework detection and `toe.toml` proposals (ADR 0013 section 5).
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
    /// Languages detected, matching skill names `toe-<language>`.
    pub languages: Vec<String>,
    /// The project's own test command, if one was found.
    pub existing: Option<String>,
    pub suites: Vec<SuiteProposal>,
    pub notes: Vec<String>,
}

struct Framework {
    language: &'static str,
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

fn detect(root: &Path) -> Vec<Framework> {
    let mut out = Vec::new();
    if root.join("Cargo.toml").exists() {
        out.push(Framework {
            language: "rust",
            run: "cargo test",
            select: "cargo test -- {filter}",
            why: "Cargo.toml; libtest output is recognized live".into(),
        });
    }
    if root.join("go.mod").exists() {
        out.push(Framework {
            language: "go",
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
            run: "python3 -m pytest -v -rfE",
            select: "python3 -m pytest -v -rfE -k {filter}",
            why: "pytest config; -v lines are recognized live".into(),
        });
    }
    let pkg = read(root, "package.json");
    if pkg.contains("\"vitest\"") {
        out.push(Framework {
            language: "ts",
            run: "npx vitest run --reporter=verbose",
            select: "npx vitest run --reporter=verbose -t {filter}",
            why: "vitest in package.json".into(),
        });
    } else if pkg.contains("\"jest\"") {
        out.push(Framework {
            language: "ts",
            run: "npx jest --ci",
            select: "npx jest --ci -t {filter}",
            why: "jest in package.json; add jest-junit for per-test results".into(),
        });
    }
    if root.join("pom.xml").exists() {
        out.push(Framework {
            language: "java",
            run: "mvn -q test",
            select: "mvn -q test -Dtest={filter}",
            why: "pom.xml; surefire reports are ingested".into(),
        });
    } else if root.join("build.gradle").exists() || root.join("build.gradle.kts").exists() {
        out.push(Framework {
            language: "java",
            run: "./gradlew test",
            select: "./gradlew test --tests {filter}",
            why: "Gradle; build/test-results are ingested".into(),
        });
    }
    if any_file_with_ext(root, "sln") || any_file_with_ext(root, "csproj") {
        out.push(Framework {
            language: "csharp",
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
    let frameworks = detect(root);
    let existing = existing_command(root);
    let mut p = Proposal {
        languages: frameworks.iter().map(|f| f.language.to_owned()).collect(),
        existing: existing.clone(),
        ..Proposal::default()
    };
    match (frameworks.as_slice(), &existing) {
        ([], None) => p
            .notes
            .push("no test framework or test command found; use `toe run -- <command>`".into()),
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
            let base: String = f
                .run
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ");
            let thin = existing.as_deref() == Some("make test")
                && recipe.len() == 1
                && (recipe[0].starts_with(&base)
                    || (f.language == "python" && recipe[0].contains("pytest")));
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
                p.suites.push(SuiteProposal {
                    name: "default".into(),
                    run: existing.clone().unwrap_or_else(|| f.run.into()),
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
        "# Proposed by `toe adopt`. Verify selection with:\n#   toe adopt --verify <suite> --filter <test name>\n",
    );
    for suite in &p.suites {
        s.push_str(&format!(
            "\n# {}\n[suite.{}]\nrun = {:?}\n",
            suite.why, suite.name, suite.run
        ));
        if let Some(sel) = &suite.select {
            s.push_str(&format!("select = {sel:?}\n"));
        }
        // The project's own command hides detail; offer the opt-in recipe.
        if p.existing.as_deref() == Some(suite.run.as_str()) {
            for (lang, recipe, what) in [
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
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("toe-adopt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for (f, c) in files {
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
