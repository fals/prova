//! Opt-in environment injection (ADR 0013 section 6, issue #33).
//!
//! A recipe adds environment to the process under test so the framework
//! reports more, without editing the project's scripts. Recipes run only
//! when listed in a suite's `inject` (or `toe run --inject`), are recorded
//! with the run, and append to any value the user already set.

use std::path::Path;

pub struct Recipe {
    pub name: &'static str,
    pub summary: &'static str,
    /// Environment variable and the value to append for a run directory.
    apply: fn(&Path) -> (&'static str, String),
}

/// Verified recipes only: each was checked against the real framework.
pub const RECIPES: &[Recipe] = &[
    Recipe {
        name: "pytest-junit",
        summary: "pytest writes a JUnit report toe ingests (PYTEST_ADDOPTS --junitxml)",
        apply: |dir| {
            (
                "PYTEST_ADDOPTS",
                format!("--junitxml={}", dir.join("pytest-junit.xml").display()),
            )
        },
    },
    Recipe {
        name: "go-nocache",
        summary: "go test reruns tests instead of replaying cached results (GOFLAGS -count=1)",
        apply: |_| ("GOFLAGS", "-count=1".to_owned()),
    },
];

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
    for r in RECIPES.iter().filter(|r| names.iter().any(|n| n == r.name)) {
        let (key, add) = (r.apply)(dir);
        let base = out
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .or_else(|| current(key))
            .filter(|v| !v.trim().is_empty());
        let value = match base {
            Some(b) => format!("{b} {add}"),
            None => add,
        };
        out.retain(|(k, _)| k != key);
        out.push((key.to_owned(), value));
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
        assert!(
            validate(&["shims".into()])
                .unwrap_err()
                .contains("known: pytest-junit, go-nocache")
        );
        assert!(
            super::env(&[], dir, |_| None).is_empty(),
            "nothing unless opted in"
        );
    }
}
