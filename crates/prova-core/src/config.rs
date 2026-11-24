//! `prova.toml` (optional, ADR 0013) and the run specification derived from
//! it or from a bare command.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{parse_duration, sha256_hex};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("prova does not read {0}: rename it to prova.toml")]
    OldName(PathBuf),
    #[error("reading {0}: {1}")]
    Read(PathBuf, std::io::Error),
    #[error("{0}: {1}")]
    Parse(PathBuf, toml::de::Error),
    #[error("suite {suite}: invalid duration {value:?} for {field}")]
    Duration {
        suite: String,
        field: &'static str,
        value: String,
    },
    #[error("no suite named {0:?} in prova.toml (known: {1})")]
    UnknownSuite(String, String),
    #[error("suite {0} has no `select` template; cannot run a single test{1}")]
    NoSelect(String, String),
    #[error(
        "invalid selection {0:?}: must be non-empty, not start with '-', no control characters"
    )]
    BadSelection(String),
}

impl ConfigError {
    /// The user's mistake in prova.toml or on the command line (exit 2),
    /// rather than a failure to read the file (exit 4).
    pub fn is_usage(&self) -> bool {
        !matches!(self, ConfigError::Read(..))
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    Small,
    Medium,
    Large,
}

impl Size {
    pub fn as_str(self) -> &'static str {
        match self {
            Size::Small => "small",
            Size::Medium => "medium",
            Size::Large => "large",
        }
    }

    pub fn parse(s: &str) -> Option<Size> {
        match s {
            "small" => Some(Size::Small),
            "medium" => Some(Size::Medium),
            "large" => Some(Size::Large),
            _ => None,
        }
    }

    /// Default idle budget before a stall is reported for a whole run.
    pub fn stall_after(self) -> Duration {
        match self {
            Size::Small => Duration::from_secs(30),
            Size::Medium => Duration::from_secs(60),
            Size::Large => Duration::from_secs(300),
        }
    }
    /// Per-test slow budget used until history exists (ADR 0005).
    pub fn slow_budget(self) -> Duration {
        match self {
            Size::Small => Duration::from_secs(1),
            Size::Medium => Duration::from_secs(30),
            Size::Large => Duration::from_secs(300),
        }
    }
    /// Per-test hard ceiling (ADR 0005, Bazel mapping).
    pub fn ceiling(self) -> Duration {
        match self {
            Size::Small => Duration::from_secs(60),
            Size::Medium => Duration::from_secs(300),
            Size::Large => Duration::from_secs(900),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Extra environment variable names to record (values are stored).
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub suite: BTreeMap<String, SuiteConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteConfig {
    /// Shell command, run from the workspace root.
    pub run: String,
    /// Template running a subset: `{filter}` is replaced by the selection.
    pub select: Option<String>,
    pub size: Option<Size>,
    pub stall_after: Option<String>,
    pub kill_after: Option<String>,
    /// Extra result file globs, relative to the workspace root.
    #[serde(default)]
    pub results: Vec<String>,
    /// Framework stream adapter, e.g. `go-json` (level 3).
    pub adapter: Option<String>,
    /// Opt-in environment injection recipes (ADR 0013 section 6), e.g.
    /// `["pytest-junit"]`. Off unless listed; recorded with every run.
    #[serde(default)]
    pub inject: Vec<String>,
    /// `false` for a command that legitimately runs no tests (a lint step):
    /// "no test ran" is then neither a failure nor a warning (ADR 0017).
    pub expect_tests: Option<bool>,
}

/// How to start the process under test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandSpec {
    /// Executed directly, no shell.
    Argv(Vec<String>),
    /// Executed with `sh -c`; comes from `prova.toml`, owned by the project.
    Shell(String),
}

impl CommandSpec {
    pub fn display(&self) -> String {
        match self {
            CommandSpec::Argv(a) => a.join(" "),
            CommandSpec::Shell(s) => s.clone(),
        }
    }
}

/// Immutable inputs of a run, stored as JSON with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunSpec {
    pub command: CommandSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Size>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<String>,
    pub stall_after_ms: u64,
    pub kill_after_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inject: Vec<String>,
    /// Keep the whole output, compressed, not only the excerpt (ADR 0016).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_output: bool,
    /// `Some(false)`: no test is expected (ADR 0017).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_tests: Option<bool>,
}

impl RunSpec {
    /// Whether "no test ran" is suspicious for this run.
    pub fn expects_tests(&self) -> bool {
        self.expect_tests != Some(false)
    }
}

/// A quiet run of a suite with no declared size stalls after this long, as
/// a small one does: `--until first-failure` (30 s) then catches a hang
/// on its second wait at the latest (round 14: 60 s, two waits or more).
pub const DEFAULT_STALL_AFTER: Duration = Duration::from_secs(30);
pub const DEFAULT_KILL_AFTER: Duration = Duration::from_secs(3600);

/// Workspace root: nearest ancestor with `prova.toml`, else with `.git`,
/// else `start` itself.
pub fn find_root(start: &Path) -> PathBuf {
    for marker in ["prova.toml", ".git"] {
        if let Some(dir) = start.ancestors().find(|d| d.join(marker).exists()) {
            return dir.to_path_buf();
        }
    }
    start.to_path_buf()
}

pub fn load(root: &Path) -> Result<Config, ConfigError> {
    let path = root.join("prova.toml");
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| ConfigError::Read(path.clone(), e))?;
    toml::from_str(&text).map_err(|e| ConfigError::Parse(path, e))
}

/// Implicit suite name for a bare command (ADR 0013): same command in the
/// same directory is the same suite.
pub fn implicit_suite(argv: &[String], rel_cwd: &str) -> String {
    let key = format!("{rel_cwd}\0{}", argv.join("\0"));
    format!("cmd:{}", &sha256_hex(key.as_bytes())[..8])
}

fn dur(
    suite: &str,
    field: &'static str,
    v: &Option<String>,
    default: Duration,
) -> Result<u64, ConfigError> {
    match v {
        None => Ok(default.as_millis() as u64),
        Some(s) => parse_duration(s)
            .map(|d| d.as_millis() as u64)
            .ok_or_else(|| ConfigError::Duration {
                suite: suite.into(),
                field,
                value: s.clone(),
            }),
    }
}

/// A test named the way prova prints it (`calc.CalcTest::divides`,
/// `example.com/calc::TestAdd`), written in the syntax of the select
/// command's framework (round 10: Maven got prova's name unchanged and ran
/// nothing). Framework syntax already given passes through unchanged.
pub fn framework_filter(command: &str, filter: &str) -> String {
    if command.contains(" -run ") || command.contains(" -run=") {
        // A pattern the user wrote (`^TestDiv`) passes through.
        if filter.starts_with('^') {
            return filter.to_owned();
        }
        let leaf = filter.rsplit_once("::").map_or(filter, |(_, l)| l);
        return go_run_pattern(leaf);
    }
    // A parameterized case (`divides(int, int, int)[2]`, `Adds(a: 1)`)
    // selects its method: Surefire and VSTest filters name methods.
    let method = |leaf: &str| -> String {
        let cut = leaf.find(['(', '[']).unwrap_or(leaf.len());
        leaf[..cut].trim_end().to_owned()
    };
    if command.contains("FullyQualifiedName") && !filter.contains("::") {
        // VSTest names a case `Ns.Class.Method(args)`.
        return method(filter);
    }
    let Some((head, leaf)) = filter.rsplit_once("::") else {
        return filter.to_owned();
    };
    if command.contains("-Dtest=") {
        // Surefire: Class#method (package-qualified class is fine).
        format!("{head}#{}", method(leaf))
    } else if command.contains("FullyQualifiedName") {
        format!("{head}.{}", method(leaf))
    } else if command.contains(" -t ") {
        // vitest -t matches the full name, describe blocks joined by spaces
        // (round 11: prova's `calc > divides` matched nothing).
        leaf.replace(" > ", " ")
    } else if command.contains(" -k ") {
        // pytest -k: the test's own name.
        leaf.to_owned()
    } else {
        filter.to_owned()
    }
}

/// `go test -run` takes one regular expression per subtest level, split
/// on `/`: each level of the name becomes a literal, anchored pattern, so
/// exactly that test runs (`TestEcho` no longer runs `TestEchoClient`, and
/// a `(` in a subtest name is not an invalid regexp).
fn go_run_pattern(name: &str) -> String {
    name.split('/')
        .map(|level| {
            let mut p = String::from("^");
            for c in level.chars() {
                if "\\.+*?()|[]{}^$".contains(c) {
                    p.push('\\');
                }
                p.push(c);
            }
            p.push('$');
            p
        })
        .collect::<Vec<_>>()
        .join("/")
}

impl Config {
    pub fn suite(&self, name: &str) -> Result<&SuiteConfig, ConfigError> {
        self.suite.get(name).ok_or_else(|| {
            let known: Vec<&str> = self.suite.keys().map(String::as_str).collect();
            ConfigError::UnknownSuite(
                name.into(),
                if known.is_empty() {
                    "none".into()
                } else {
                    known.join(", ")
                },
            )
        })
    }

    pub fn run_spec(&self, name: &str, selection: Option<&str>) -> Result<RunSpec, ConfigError> {
        let s = self.suite(name)?;
        let command = match selection {
            None => s.run.clone(),
            Some(f) => {
                if f.is_empty() || f.starts_with('-') || f.chars().any(char::is_control) {
                    return Err(ConfigError::BadSelection(f.into()));
                }
                // The filter is passed in PROVA_FILTER and expanded by the shell
                // as one quoted word; its text never becomes shell syntax.
                s.select
                    .as_ref()
                    .ok_or_else(|| {
                        // Point to a suite that can (round 8).
                        let other: Vec<&String> = self
                            .suite
                            .iter()
                            .filter(|(n, x)| *n != name && x.select.is_some())
                            .map(|(n, _)| n)
                            .collect();
                        ConfigError::NoSelect(
                            name.into(),
                            other
                                .first()
                                .map(|o| {
                                    format!("; suite `{o}` can: prova run {o} --select <test>")
                                })
                                .unwrap_or_default(),
                        )
                    })?
                    .replace("{filter}", "\"$PROVA_FILTER\"")
            }
        };
        let default_stall = s.size.map(Size::stall_after).unwrap_or(DEFAULT_STALL_AFTER);
        Ok(RunSpec {
            command: CommandSpec::Shell(command),
            size: s.size,
            selection: selection.map(str::to_owned),
            stall_after_ms: dur(name, "stall_after", &s.stall_after, default_stall)?,
            kill_after_ms: dur(name, "kill_after", &s.kill_after, DEFAULT_KILL_AFTER)?,
            results: s.results.clone(),
            adapter: s.adapter.clone(),
            inject: s.inject.clone(),
            keep_output: false,
            expect_tests: s.expect_tests,
        })
    }
}

#[cfg(test)]
mod filter_tests {
    /// Owner's e2e suite: `go test -run` takes a regular expression per
    /// subtest level, so a name was a pattern: `(` made an invalid regexp,
    /// `.` matched anything, and `TestEcho` also ran `TestEchoClient`.
    #[test]
    fn a_go_test_name_selects_exactly_that_test() {
        use super::framework_filter as f;
        let go = "go test -json -run \"$PROVA_FILTER\" ./tests/scenarios -args --parallel";
        assert_eq!(f(go, "TestEcho"), "^TestEcho$");
        assert_eq!(
            f(go, "TestPod/pod_recovers_on_timeout(5s)/wait_for_v1.2+#01"),
            r"^TestPod$/^pod_recovers_on_timeout\(5s\)$/^wait_for_v1\.2\+#01$"
        );
        assert_eq!(
            f(go, "example.com/clm/tests::TestA/case_[x]|y"),
            r"^TestA$/^case_\[x\]\|y$"
        );
        assert_eq!(f("go test -run=$PROVA_FILTER ./...", "TestB"), "^TestB$");
        // A pattern the user wrote is theirs.
        assert_eq!(f(go, "^TestEcho"), "^TestEcho");
    }

    /// Round 13 (Java, C#): a parameterized case named as prova shows it
    /// (`divides(int, int, int)[2]`, `LengthMatches(s: "x/y", len: 3)`)
    /// matched nothing: Surefire selects `Class#method`, VSTest filters
    /// by method name. The case selects its method (all of its cases).
    #[test]
    fn a_parameterized_case_selects_its_method() {
        use super::framework_filter as f;
        assert_eq!(
            f(
                "mvn -q test -Dtest=\"$PROVA_FILTER\"",
                "calc.CalcParamTest::divides(int, int, int)[2]"
            ),
            "calc.CalcParamTest#divides"
        );
        assert_eq!(
            f(
                "dotnet test --filter \"FullyQualifiedName~$PROVA_FILTER\"",
                "Calc.TheoryTests.LengthMatches(s: \"x/y\", len: 3)"
            ),
            "Calc.TheoryTests.LengthMatches"
        );
        assert_eq!(
            f(
                "dotnet test --filter \"FullyQualifiedName~$PROVA_FILTER\"",
                "Calc.TheoryTests::LengthMatches(s: \"x/y\", len: 3)"
            ),
            "Calc.TheoryTests.LengthMatches"
        );
    }

    #[test]
    fn prova_names_become_framework_filters() {
        use super::framework_filter as f;
        assert_eq!(
            f(
                "mvn -q test -Dtest=\"$PROVA_FILTER\"",
                "calc.CalcTest::divides"
            ),
            "calc.CalcTest#divides"
        );
        assert_eq!(
            f(
                "dotnet test --filter FullyQualifiedName~x",
                "Calc.CalcTests::Adds"
            ),
            "Calc.CalcTests.Adds"
        );
        assert_eq!(
            f(
                "go test -json -run \"$PROVA_FILTER\" ./...",
                "example.com/calc::TestAdd"
            ),
            "^TestAdd$"
        );
        assert_eq!(
            f("mvn -q test -Dtest=\"$PROVA_FILTER\"", "CalcTest#divides"),
            "CalcTest#divides"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suite_with_selection_uses_template() {
        let c: Config = toml::from_str(
            r#"
            [suite.small]
            run = "make test"
            select = "make test TEST={filter}"
            size = "small"
            "#,
        )
        .unwrap();
        let spec = c.run_spec("small", Some("parser")).unwrap();
        assert_eq!(
            spec.command,
            CommandSpec::Shell("make test TEST=\"$PROVA_FILTER\"".into())
        );
        assert_eq!(spec.selection.as_deref(), Some("parser"));
        assert!(matches!(
            c.run_spec("small", Some("-x")),
            Err(ConfigError::BadSelection(_))
        ));
        assert_eq!(spec.stall_after_ms, 30_000);
        assert!(matches!(
            c.run_spec("nope", None),
            Err(ConfigError::UnknownSuite(..))
        ));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("[suite.a]\nrun = \"x\"\ntimeout = 3\n").is_err());
    }

    #[test]
    fn implicit_suite_depends_on_command_and_dir() {
        let a = implicit_suite(&["make".into(), "test".into()], ".");
        assert_eq!(a, implicit_suite(&["make".into(), "test".into()], "."));
        assert_ne!(a, implicit_suite(&["make".into(), "check".into()], "."));
        assert_ne!(a, implicit_suite(&["make".into(), "test".into()], "sub"));
    }
}
