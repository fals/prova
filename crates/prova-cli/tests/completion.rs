//! Shell completion: what a TAB offers. Medium: the real binary, a
//! prova.toml and a store on disk.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

const PROVA: &str = env!("CARGO_BIN_EXE_prova");

fn workspace(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("prova-compl-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("repo")).unwrap();
    d
}

/// The candidates bash would get for `prova <words...>` (the last word is
/// the one being completed), through clap's completion protocol.
fn complete(d: &Path, words: &[&str]) -> Vec<String> {
    let mut args = vec!["--", "prova"];
    args.extend_from_slice(words);
    let o = Command::new(PROVA)
        .args(&args)
        .current_dir(d.join("repo"))
        .env("PROVA_HOME", d.join("home"))
        .env("COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", words.len().to_string())
        .env("_CLAP_COMPLETE_COMP_TYPE", "9")
        .env("_CLAP_COMPLETE_SPACE", "false")
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

#[test]
fn tab_offers_suites_and_runs_and_creates_nothing() {
    let d = workspace("tab");
    let repo = d.join("repo");
    std::fs::write(
        repo.join("prova.toml"),
        "[suite.unit]\nrun = \"true\"\n[suite.integration]\nrun = \"true\"\n",
    )
    .unwrap();
    // No store yet: run ids offer only `last`, and nothing is created.
    assert_eq!(complete(&d, &["output", ""])[0], "last");
    assert!(!d.join("home").exists(), "a TAB created the prova home");

    let suites = complete(&d, &["run", ""]);
    assert!(
        suites.contains(&"unit".to_owned()) && suites.contains(&"integration".to_owned()),
        "{suites:?}"
    );
    assert_eq!(
        complete(&d, &["run", "unit", "--until", ""]),
        ["first-failure", "stall", "done"]
    );

    let o = Command::new(PROVA)
        .args(["run", "unit", "--json"])
        .current_dir(&repo)
        .env("PROVA_HOME", d.join("home"))
        .output()
        .unwrap();
    let id = serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["run"]
        .as_str()
        .unwrap()
        .to_owned();
    let runs = complete(&d, &["output", ""]);
    assert!(runs.contains(&id), "{runs:?}");
}
