//! Opt-in environment injection (ADR 0013 section 6, issue #33).
//!
//! A recipe adds environment to the process under test so the framework
//! reports more, without editing the project's scripts. Recipes run only
//! when listed in a suite's `inject` (or `prova run --inject`), are recorded
//! with the run, and append to any value the user already set.

use std::path::Path;

pub struct Recipe {
    pub name: &'static str,
    pub summary: &'static str,
    /// Environment for a run directory: (variable, value, append?). Appended
    /// values join the user's own value with a space; others replace it.
    env: fn(&Path) -> Vec<(&'static str, String, bool)>,
    /// Files written into the run directory first: (name, content).
    files: &'static [(&'static str, &'static str)],
    /// A file in the run directory the framework appends events to while it
    /// runs; prova reads it live, like output.
    pub stream: Option<&'static str>,
}

/// Verified recipes only: each was checked against the real framework.
pub const RECIPES: &[Recipe] = &[
    Recipe {
        name: "pytest-junit",
        summary: "pytest writes a JUnit report prova ingests (PYTEST_ADDOPTS --junitxml)",
        env: |dir| {
            vec![(
                "PYTEST_ADDOPTS",
                format!("--junitxml={}", dir.join("pytest-junit.xml").display()),
                true,
            )]
        },
        files: &[],
        stream: None,
    },
    Recipe {
        name: "pytest-reportlog",
        summary: "pytest streams one event per test phase, read live (PYTEST_ADDOPTS --report-log; needs pytest-reportlog)",
        env: |dir| {
            vec![(
                "PYTEST_ADDOPTS",
                format!(
                    "--report-log={}",
                    dir.join("pytest-reportlog.jsonl").display()
                ),
                true,
            )]
        },
        files: &[],
        stream: Some("pytest-reportlog.jsonl"),
    },
    Recipe {
        name: "vitest-stream",
        summary: "prova's Vitest reporter streams every test live; add --reporter=default --reporter=\"$PROVA_VITEST_REPORTER\" to the vitest command",
        env: |dir| {
            vec![
                (
                    "PROVA_VITEST_REPORTER",
                    dir.join("prova-vitest-reporter.mjs").display().to_string(),
                    false,
                ),
                (
                    "PROVA_STREAM",
                    dir.join("vitest.jsonl").display().to_string(),
                    false,
                ),
            ]
        },
        files: &[(
            "prova-vitest-reporter.mjs",
            include_str!("vitest-reporter.mjs"),
        )],
        stream: Some("vitest.jsonl"),
    },
    Recipe {
        name: "gotestsum-json",
        summary: "gotestsum writes go test's own event stream, read live (GOTESTSUM_JSONFILE); its text output is then ignored",
        env: |dir| {
            vec![(
                "GOTESTSUM_JSONFILE",
                dir.join("gotestsum.jsonl").display().to_string(),
                false,
            )]
        },
        files: &[],
        stream: Some("gotestsum.jsonl"),
    },
    Recipe {
        name: "go-nocache",
        summary: "go test reruns tests instead of replaying cached results (GOFLAGS -count=1)",
        env: |_| vec![("GOFLAGS", "-count=1".to_owned(), true)],
        files: &[],
        stream: None,
    },
];

fn chosen(names: &[String]) -> impl Iterator<Item = &'static Recipe> + '_ {
    RECIPES.iter().filter(|r| names.iter().any(|n| n == r.name))
}

/// Files the listed recipes need in the run directory.
pub fn files(names: &[String]) -> Vec<(&'static str, &'static str)> {
    chosen(names)
        .flat_map(|r| r.files.iter().copied())
        .collect()
}

/// Stream files the listed recipes make the framework write.
pub fn streams(names: &[String]) -> Vec<&'static str> {
    chosen(names).filter_map(|r| r.stream).collect()
}

pub fn names() -> String {
    RECIPES
        .iter()
        .map(|r| r.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Rejects unknown recipe names before a run is created.
pub fn validate(names: &[String]) -> Result<(), String> {
    for n in names {
        if !RECIPES.iter().any(|r| r.name == n) {
            return Err(format!(
                "unknown inject recipe {n:?}; known: {}",
                self::names()
            ));
        }
    }
    Ok(())
}

/// Environment for the listed recipes. `current` reads the caller's
/// environment so existing values are kept, with the recipe appended.
pub fn env(
    names: &[String],
    dir: &Path,
    current: impl Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for r in chosen(names) {
        for (key, add, append) in (r.env)(dir) {
            let base = out
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .or_else(|| current(key))
                .filter(|v| append && !v.trim().is_empty());
            let value = match base {
                Some(b) => format!("{b} {add}"),
                None => add,
            };
            out.retain(|(k, _)| k != key);
            out.push((key.to_owned(), value));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_to_existing_values_and_rejects_unknown() {
        let dir = Path::new("/r");
        let env = env(&["pytest-junit".into(), "go-nocache".into()], dir, |k| {
            (k == "GOFLAGS").then(|| "-mod=mod".to_owned())
        });
        assert_eq!(
            env,
            vec![
                (
                    "PYTEST_ADDOPTS".into(),
                    "--junitxml=/r/pytest-junit.xml".into()
                ),
                ("GOFLAGS".into(), "-mod=mod -count=1".into()),
            ]
        );
        assert!(validate(&["pytest-junit".into()]).is_ok());
        assert!(validate(&["shims".into()]).unwrap_err().contains(
            "known: pytest-junit, pytest-reportlog, vitest-stream, gotestsum-json, go-nocache"
        ));
        assert!(
            super::env(&[], dir, |_| None).is_empty(),
            "nothing unless opted in"
        );
        let v = vec!["vitest-stream".to_string()];
        assert_eq!(
            super::env(&v, dir, |_| Some("user value".into())),
            vec![
                (
                    "PROVA_VITEST_REPORTER".into(),
                    "/r/prova-vitest-reporter.mjs".into()
                ),
                ("PROVA_STREAM".into(), "/r/vitest.jsonl".into()),
            ],
            "set, not appended"
        );
        assert_eq!(files(&v)[0].0, "prova-vitest-reporter.mjs");
        assert_eq!(streams(&v), vec!["vitest.jsonl"]);
    }
}
