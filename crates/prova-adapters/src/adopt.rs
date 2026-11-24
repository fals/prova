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
    /// The project's own test command runs gotestsum: its event stream can
    /// be injected (`gotestsum-json`).
    pub gotestsum: bool,
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
            run: "dotnet test --logger trx --logger \"console;verbosity=normal\"",
            select: "dotnet test --logger trx --logger \"console;verbosity=normal\" --filter FullyQualifiedName~{filter}",
            why: ".NET; TRX results are ingested, and the console logger prints each test as it finishes so a hang names the running test".into(),
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

/// The steps of `npm test` besides the test runner: the other commands of
/// the `test` script (`tsc --noEmit && vitest run`) and its `pretest` and
/// `posttest` scripts. A selected run starts the runner alone.
fn npm_test_extra_steps(root: &Path, runner: &str) -> Vec<String> {
    let pkg: serde_json::Value =
        serde_json::from_str(&read(root, "package.json")).unwrap_or_default();
    let scripts = &pkg["scripts"];
    let mut steps = Vec::new();
    let mut parts = |script: &str| {
        for part in script.split("&&").flat_map(|x| x.split(';')) {
            let part = part.trim();
            if !part.is_empty() && !part.contains(runner) {
                steps.push(part.to_owned());
            }
        }
    };
    if let Some(t) = scripts["pretest"].as_str() {
        parts(t);
    }
    if let Some(t) = scripts["test"].as_str() {
        parts(t);
    }
    if let Some(t) = scripts["posttest"].as_str() {
        parts(t);
    }
    steps
}

pub fn propose(root: &Path) -> Proposal {
    let mut p = propose_with(root, nextest_version);
    p.reportlog = p.languages.iter().any(|l| l == "python") && pytest_reportlog_installed(root);
    p
}

/// Test runners and entry points a wrapper puts in front of.
const RUNNERS: &[&str] = &[
    "make",
    "pytest",
    "python",
    "python3",
    "py.test",
    "mvn",
    "mvnw",
    "gradle",
    "gradlew",
    "npm",
    "npx",
    "yarn",
    "pnpm",
    "bun",
    "cargo",
    "go",
    "gotestsum",
    "dotnet",
    "vitest",
    "jest",
    "rake",
    "bundle",
    "tox",
    "nox",
];

/// A wrapper in front of the test command, and how it takes it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Wrapper {
    pub prefix: String,
    /// The command goes as one quoted argument (`./dev "dotnet test"`, a
    /// wrapper that runs `sh -c "$*"`), not as separate words.
    pub quoted: bool,
}

fn is_runner(w: &str) -> bool {
    RUNNERS.contains(&w.rsplit('/').next().unwrap_or(w))
}

/// The wrapper in front of a test command an earlier run used (`./dev`,
/// `docker compose run web`, `nix develop -c`): everything before the first
/// word that is a test runner. None for a command that starts with its
/// runner, or with a shell, whose arguments are not a prefix.
pub fn wrapper_of(argv: &[String]) -> Option<String> {
    wrapper_form(argv).filter(|w| !w.quoted).map(|w| w.prefix)
}

/// Like `wrapper_of`, also for a wrapper given the whole command as one
/// quoted argument (`./dev "dotnet test"`: round 16, adopt dropped `./dev`).
pub fn wrapper_form(argv: &[String]) -> Option<Wrapper> {
    let name = |w: &str| w.rsplit('/').next().unwrap_or(w).to_owned();
    if let [wrapper, whole] = argv
        && whole.contains(' ')
        && !wrapper.contains(['"', '\'', '=', ' '])
        && !is_runner(wrapper)
        && !matches!(name(wrapper).as_str(), "sh" | "bash" | "zsh" | "dash")
        && whole.split_whitespace().next().is_some_and(is_runner)
    {
        return Some(Wrapper {
            prefix: wrapper.clone(),
            quoted: true,
        });
    }
    let at = argv.iter().position(|w| is_runner(w))?;
    let prefix = &argv[..at];
    let shell = |w: &String| matches!(name(w).as_str(), "sh" | "bash" | "zsh" | "dash");
    if prefix.is_empty()
        || prefix.iter().any(shell)
        || prefix.iter().any(|w| w.contains(['"', '\'', '=', ' ']))
    {
        return None;
    }
    Some(Wrapper {
        prefix: prefix.join(" "),
        quoted: false,
    })
}

/// The wrapper of a command line a suite runs with `sh -c` (a `prova.toml`
/// `run`): its words are split at spaces, a trailing quoted argument kept
/// whole.
pub fn wrapper_of_shell(cmd: &str) -> Option<Wrapper> {
    let (head, rest) = match cmd.find(['"', '\'']) {
        Some(at) => (&cmd[..at], Some(&cmd[at..])),
        None => (cmd, None),
    };
    if head.contains(['&', ';', '|', '`', '$', '\n', '=']) {
        return None;
    }
    let mut argv: Vec<String> = head.split_whitespace().map(str::to_owned).collect();
    if let Some(rest) = rest {
        let quote = rest.chars().next()?;
        let inner = rest.strip_prefix(quote)?.strip_suffix(quote)?;
        if inner.contains(quote) {
            return None;
        }
        argv.push(inner.to_owned());
    }
    wrapper_form(&argv)
}

/// Puts the project's wrapper in front of every suite's command and select
/// (round 15: adopt proposed `python3 -m pytest` on the host when the
/// project's own runs went through `./dev`).
pub fn apply_wrapper(p: &mut Proposal, wrapper: &str) {
    apply_wrapper_form(
        p,
        &Wrapper {
            prefix: wrapper.to_owned(),
            quoted: false,
        },
    );
}

/// `apply_wrapper` for either way a wrapper takes its command.
pub fn apply_wrapper_form(p: &mut Proposal, w: &Wrapper) {
    let wrapper = w.prefix.as_str();
    let quote = |cmd: &str| {
        // Inside double quotes: the quotes and backslashes of the command
        // itself are escaped; `$` stays, so `{filter}` and variables expand
        // as before. The wrapper's `sh -c "$*"` parses the quotes again.
        let escaped = cmd
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('`', "\\`");
        format!("{wrapper} \"{escaped}\"")
    };
    let wrap = |cmd: &str| {
        if cmd.starts_with(wrapper) {
            return cmd.to_owned();
        }
        // A quoted `;` would split at the wrapper's `$*`: the console
        // logger's `"console;verbosity=normal"` goes inside the quotes.
        if w.quoted || cmd.contains("\"console;") {
            return quote(cmd);
        }
        // A compound command or one that starts with variable assignments
        // cannot take a prefix as is.
        let first = cmd.split_whitespace().next().unwrap_or("");
        if cmd.contains(';') || first.contains('=') {
            cmd.to_owned()
        } else {
            format!("{wrapper} {cmd}")
        }
    };
    for s in &mut p.suites {
        s.run = wrap(&s.run);
        s.select = s.select.as_deref().map(wrap);
    }
    p.existing = p.existing.as_deref().map(wrap);
    // Whether the host has pytest-reportlog says nothing about the wrapper.
    p.reportlog = false;
    p.notes.push(if w.quoted {
        format!(
            "commands run through `{wrapper}`, as your earlier runs did, with the whole command as one quoted argument; `{{filter}}` stays inside that argument"
        )
    } else {
        format!(
            "commands run through `{wrapper}`, as your earlier runs did; a wrapper that re-splits its arguments (`$*`) breaks a filter with spaces: select with a substring that has none"
        )
    });
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
        gotestsum: make_test_recipe(root)
            .iter()
            .any(|l| l.contains("gotestsum"))
            || read(root, "package.json").contains("gotestsum"),
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
                    why: "the project's own command, as developers and CI run it".into(),
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
                // Steps of the project's test script besides the runner:
                // `select` runs the runner alone, so they are skipped there.
                let skipped = if existing.as_deref() == Some("npm test") {
                    npm_test_extra_steps(root, f.base)
                } else {
                    Vec::new()
                };
                if !skipped.is_empty() {
                    p.notes.push(format!(
                        "select runs {} alone: {} from `npm test` {} not run for single tests (a failure there passes under --select)",
                        f.base,
                        skipped
                            .iter()
                            .map(|s| format!("`{s}`"))
                            .collect::<Vec<_>>()
                            .join(", "),
                        if skipped.len() == 1 { "does" } else { "do" },
                    ));
                }
                p.suites.push(SuiteProposal {
                    name: "default".into(),
                    run,
                    select: Some(f.select.into()),
                    why: match &existing {
                        Some(cmd) if f.language == "python" => format!(
                            "runs `{cmd}` as the project does; passes are seen live only if its pytest runs verbose (-v) or with pytest-reportlog (inject = [\"pytest-reportlog\"]); selection runs `{}` verbosely",
                            f.select.split(" -k").next().unwrap_or(f.select)
                        ),
                        Some(cmd) if !skipped.is_empty() => format!(
                            "runs `{cmd}` as the project does; selection via {} (select runs {} alone: `{}` does not run for single tests)",
                            f.why,
                            f.base,
                            skipped.join("`, `")
                        ),
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
        // gotestsum is already what the project runs: its own event stream
        // needs nothing installed, so it is proposed on, like the Vitest
        // reporter (round 14: left commented, the suite saw only failures).
        if p.gotestsum && p.existing.as_deref() == Some(suite.run.as_str()) {
            s.push_str(
                "# gotestsum prints only failures by default; its event stream gives every test, live, with packages\ninject = [\"gotestsum-json\"]\n# also opt-in: \"go-nocache\" (stop go replaying cached passes via GOFLAGS)\n",
            );
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
    fn wrappers_of_earlier_runs_are_kept() {
        let v = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(wrapper_of(&v("./dev make test")).as_deref(), Some("./dev"));
        assert_eq!(
            wrapper_of(&v("docker compose run web pytest -q")).as_deref(),
            Some("docker compose run web")
        );
        assert_eq!(
            wrapper_of(&v("nix develop -c cargo test")).as_deref(),
            Some("nix develop -c")
        );
        assert_eq!(wrapper_of(&v("make test")), None);
        assert_eq!(wrapper_of(&v("sh -c make")), None);
    }

    #[test]
    fn a_wrapper_goes_in_front_of_run_and_select() {
        let d = dir(
            "wrapped-py",
            &[
                ("pyproject.toml", "[tool.pytest.ini_options]\n"),
                ("t.py", ""),
            ],
        );
        let mut p = propose(&d);
        apply_wrapper(&mut p, "./dev");
        for s in &p.suites {
            assert!(s.run.starts_with("./dev "), "{s:?}");
            assert!(s.select.as_deref().is_none_or(|x| x.starts_with("./dev ")));
        }
        assert!(!p.reportlog);
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
    fn a_make_test_running_gotestsum_gets_its_stream_hint() {
        let p = propose(&dir(
            "gotestsum",
            &[
                ("go.mod", "module x"),
                ("Makefile", "test:\n\tgotestsum --format pkgname -- ./...\n"),
            ],
        ));
        assert!(p.gotestsum);
        assert!(
            to_toml(&p).contains("\ninject = [\"gotestsum-json\"]\n"),
            "{}",
            to_toml(&p)
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

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|w| (*w).to_owned()).collect()
    }

    /// Round 16: `./dev "dotnet test"` (the whole command as one argument)
    /// was no wrapper: adopt proposed the bare command.
    #[test]
    fn a_wrapper_given_the_whole_command_quoted_is_kept() {
        let w = wrapper_form(&argv(&["./dev", "dotnet test"])).unwrap();
        assert_eq!((w.prefix.as_str(), w.quoted), ("./dev", true));
        let w = wrapper_form(&argv(&["./dev", "mvn -q test"])).unwrap();
        assert!(w.quoted);
        assert_eq!(wrapper_form(&argv(&["./dev", "echo hi there"])), None);
        assert_eq!(wrapper_form(&argv(&["make", "test"])), None);
        let w = wrapper_of_shell("./dev 'mvn -q test'").unwrap();
        assert_eq!((w.prefix.as_str(), w.quoted), ("./dev", true));
        let w = wrapper_of_shell("./dev make test").unwrap();
        assert!(!w.quoted);
        assert_eq!(wrapper_of_shell("cd x && make test"), None);
        assert_eq!(wrapper_of_shell("FOO=1 cargo test"), None);
    }

    #[test]
    fn a_quoted_wrapper_wraps_the_whole_command_and_keeps_the_console_logger_whole() {
        let d = dir("csharp-wrap", &[("App.sln", "")]);
        let mut p = propose(&d);
        apply_wrapper_form(
            &mut p,
            &Wrapper {
                prefix: "./dev".into(),
                quoted: true,
            },
        );
        let run = &p.suites[0].run;
        assert!(
            run.starts_with(
                "./dev \"dotnet test --logger trx --logger \\\"console;verbosity=normal\\\"\""
            ),
            "{run}"
        );
        let sel = p.suites[0].select.as_deref().unwrap();
        assert!(sel.ends_with("FullyQualifiedName~{filter}\""), "{sel}");
        // An unquoted wrapper must not split the logger at its `;` either.
        let mut p = propose(&d);
        apply_wrapper(&mut p, "./dev");
        assert!(
            p.suites[0].run.starts_with("./dev \"dotnet test"),
            "{:?}",
            p.suites[0].run
        );
    }

    /// Round 16: the console logger shows each test as it finishes, so a
    /// hang names the running test.
    #[test]
    fn dotnet_is_proposed_with_the_console_logger() {
        let d = dir("csharp-console", &[("App.sln", "")]);
        let p = propose(&d);
        assert!(p.suites[0].run.contains("console;verbosity=normal"));
        assert!(p.suites[0].run.contains("--logger trx"));
    }

    /// Round 16 (coordinator): `tsc --noEmit && vitest run` as `npm test`;
    /// `select` runs vitest alone.
    #[test]
    fn select_that_skips_steps_of_the_test_script_says_so() {
        let d = dir(
            "npm-steps",
            &[(
                "package.json",
                r#"{"scripts":{"test":"tsc --noEmit && vitest run"},"devDependencies":{"vitest":"1"}}"#,
            )],
        );
        let p = propose(&d);
        let note = p.notes.join("\n");
        assert!(
            note.contains("select runs vitest alone") && note.contains("`tsc --noEmit`"),
            "{note}"
        );
        let toml = to_toml(&p);
        assert!(toml.contains("select runs vitest alone"), "{toml}");
        // A plain script says nothing.
        let d = dir(
            "npm-plain",
            &[(
                "package.json",
                r#"{"scripts":{"test":"vitest run"},"devDependencies":{"vitest":"1"}}"#,
            )],
        );
        assert!(!propose(&d).notes.join("\n").contains("alone"));
    }

    /// Round 16: the comment claimed `-v` lines for a command that is not
    /// verbose.
    #[test]
    fn the_project_command_is_not_claimed_verbose() {
        let d = dir(
            "py-quiet",
            &[
                ("pytest.ini", "[pytest]\n"),
                ("Makefile", "test:\n\tpytest -q\n\tflake8\n"),
            ],
        );
        let p = propose(&d);
        let why = &p.suites[0].why;
        assert!(
            why.contains("only if its pytest runs verbose")
                && !why.contains("-v lines are recognized"),
            "{why}"
        );
    }
}
