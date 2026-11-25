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
            Size::Small => Duration::from_secs(20),
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
/// a small one does: inside the first `--until first-failure` wait (30 s),
/// so a hang is named on the first call (rounds 14 and 15).
pub const DEFAULT_STALL_AFTER: Duration = Duration::from_secs(20);
/// A test with no history in a suite with no declared size is called slow
/// after this long, inside the first 30 s wait (round 15: 60 s).
pub const DEFAULT_SLOW: Duration = Duration::from_secs(20);
pub const DEFAULT_KILL_AFTER: Duration = Duration::from_secs(3600);

/// Whether an injection recipe can act on `command`: the recipes that
/// configure one tool need that tool (or a wrapper that may run it: make,
/// npm, a script) in the command.
fn recipe_applies(recipe: &str, command: &str) -> bool {
    let runs = |tool: &str| command.contains(tool);
    let indirect = ["make", "npm", "yarn", "pnpm", "./", "sh ", "bash "]
        .iter()
        .any(|w| command.contains(w));
    match recipe {
        "gotestsum-json" => runs("gotestsum") || (indirect && !runs("go test")),
        "vitest-stream" => runs("PROVA_VITEST_REPORTER"),
        _ => true,
    }
}

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
    // Vitest and Jest `-t` match the full name, describe blocks joined by
    // spaces: the name prova shows (`calc > divides`, with or without its
    // file) selects with spaces (round 14: it matched nothing).
    if command.contains(" -t ") || command.contains("--testNamePattern") {
        let leaf = split_node(filter).1;
        // `-t` is a regular expression: a name with `(1, 2)` is a literal
        // unless the user wrote a pattern (`^add`, `sub$`) (round 15).
        if leaf.starts_with('^') || leaf.ends_with('$') {
            return leaf.replace(" > ", " ");
        }
        // Vitest 5 joins describe blocks with ` > `, older Vitest and Jest
        // with a space: one pattern matches either (round 17).
        return leaf
            .split(" > ")
            .map(regex_escape)
            .collect::<Vec<_>>()
            .join("(?: > | )");
    }
    let (Some(head), leaf) = split_node(filter) else {
        return filter.to_owned();
    };
    if command.contains("-Dtest=") {
        // Surefire: Class#method (package-qualified class is fine).
        format!("{head}#{}", method(leaf))
    } else if command.contains("FullyQualifiedName") {
        format!("{head}.{}", method(leaf))
    } else if command.contains(" -k ") {
        // pytest -k: the test's own name; a parameter id with spaces
        // cannot be one `-k` word, so its test function selects (all of
        // its cases run).
        if leaf.contains(char::is_whitespace) {
            method(leaf)
        } else {
            leaf.to_owned()
        }
    } else {
        filter.to_owned()
    }
}

/// A source file extension that makes the text before `::` a file.
const FILE_EXTS: [&str; 7] = [".py", ".js", ".ts", ".jsx", ".tsx", ".mjs", ".cjs"];

/// A selection as `(head, leaf)`. A node id names its file first
/// (`tests/test_calc.py::test_param[a b-c::d]`): split after the file, so a
/// `::` inside a parameter id stays in the leaf. Otherwise the last `::`.
fn split_node(filter: &str) -> (Option<&str>, &str) {
    if let Some((head, rest)) = filter.split_once("::")
        && FILE_EXTS.iter().any(|e| head.ends_with(e))
    {
        return (Some(head), rest);
    }
    match filter.rsplit_once("::") {
        Some((h, l)) => (Some(h), l),
        None => (None, filter),
    }
}

/// A name as a literal in a JavaScript regular expression.
fn regex_escape(name: &str) -> String {
    let mut p = String::with_capacity(name.len());
    for c in name.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            p.push('\\');
        }
        p.push(c);
    }
    p
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

/// `s` as one single-quoted shell word: nothing in it expands.
pub fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A select template with `{filter}` filled in as a variable. Unquoted it
/// is `"$PROVA_FILTER"`, one word. Inside a double-quoted argument (a
/// wrapper's `./dev "pytest -k {filter}"`) a nested `"` would close the
/// argument, and the value would be parsed again by the inner shell: it
/// goes in as `$PROVA_FILTER_Q`, the value already single-quoted, so it
/// reaches the inner shell as one unexpanded word (round 17).
fn fill_filter(template: &str) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let (mut single, mut double, mut escaped) = (false, false, false);
    let mut rest = template;
    while let Some(c) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("{filter}") {
            out.push_str(if double {
                "$PROVA_FILTER_Q"
            } else {
                "\"$PROVA_FILTER\""
            });
            rest = after;
            escaped = false;
            continue;
        }
        if escaped {
            escaped = false;
        } else if c == '\\' && !single {
            escaped = true;
        } else if c == '\'' && !double {
            single = !single;
        } else if c == '"' && !single {
            double = !double;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
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
                let template = s.select.as_ref().ok_or_else(|| {
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
                            .map(|o| format!("; suite `{o}` can: prova run {o} --select <test>"))
                            .unwrap_or_default(),
                    )
                })?;
                fill_filter(template)
            }
        };
        let default_stall = s.size.map(Size::stall_after).unwrap_or(DEFAULT_STALL_AFTER);
        let inject = s
            .inject
            .iter()
            .filter(|r| selection.is_none() || recipe_applies(r, &command))
            .cloned()
            .collect();
        Ok(RunSpec {
            command: CommandSpec::Shell(command),
            size: s.size,
            selection: selection.map(str::to_owned),
            stall_after_ms: dur(name, "stall_after", &s.stall_after, default_stall)?,
            kill_after_ms: dur(name, "kill_after", &s.kill_after, DEFAULT_KILL_AFTER)?,
            results: s.results.clone(),
            adapter: s.adapter.clone(),
            // A recipe for a tool the select command does not run is not
            // injected there (round 16: gotestsum's stream for a plain `go
            // test -json` select made every selected pass "unverified").
            inject,
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

    /// Round 14: Vitest and Jest show `calc > divides`; their `-t` joins the
    /// describe blocks with a space, whether or not the file is named.
    #[test]
    fn a_vitest_or_jest_name_selects_with_spaces() {
        use super::framework_filter as f;
        let vitest = "npx vitest run --reporter=default -t \"$PROVA_FILTER\"";
        assert_eq!(f(vitest, "calc > divides"), "calc(?: > | )divides");
        assert_eq!(
            f(vitest, "src/calc.test.ts::calc > divides"),
            "calc(?: > | )divides"
        );
        assert_eq!(f(vitest, "plain"), "plain");
        assert_eq!(
            f("npx jest --ci -t \"$PROVA_FILTER\"", "a > b > c"),
            "a(?: > | )b(?: > | )c"
        );
        assert_eq!(
            f("jest --testNamePattern \"$PROVA_FILTER\"", "a > b"),
            "a(?: > | )b"
        );
    }

    /// Round 15: `-t` takes a regular expression; a test named
    /// `add(1, 2) -> 3` selected nothing.
    #[test]
    fn a_vitest_or_jest_name_is_escaped_for_the_regex() {
        use super::framework_filter as f;
        let jest = "npx jest -t \"$PROVA_FILTER\"";
        assert_eq!(f(jest, "add(1, 2) -> 3"), r"add\(1, 2\) -> 3");
        assert_eq!(
            f(jest, "src/x.test.ts::math > add(1, 2) -> 3"),
            r"math(?: > | )add\(1, 2\) -> 3"
        );
        assert_eq!(
            f(jest, "$a/b `c` 'd' %x $(rm)"),
            r"\$a/b `c` 'd' %x \$\(rm\)"
        );
        assert_eq!(f(jest, "a.b+c"), r"a\.b\+c");
        // A pattern the user wrote is theirs.
        assert_eq!(f(jest, "^add|sub$"), "^add|sub$");
    }

    /// Round 17 (Vitest 5): `-t` matches the name joined with ` > `; older
    /// versions and Jest join with spaces. One pattern matches both.
    #[test]
    fn a_nested_vitest_name_matches_in_both_joins() {
        use super::framework_filter as f;
        let vitest = "npx vitest run -t \"$PROVA_FILTER\"";
        let p = f(vitest, "suite > inner > works");
        assert_eq!(p, "suite(?: > | )inner(?: > | )works");
        assert_eq!(
            f(vitest, "src/a/x.test.js::math > ops > adds"),
            "math(?: > | )ops(?: > | )adds"
        );
    }

    /// Round 17: pytest prints node ids; `::` can also sit in a parameter id.
    #[test]
    fn a_pytest_node_id_selects_the_test_name() {
        use super::framework_filter as f;
        let py = "python3 -m pytest -v -k \"$PROVA_FILTER\"";
        assert_eq!(f(py, "tests/test_calc.py::test_ok"), "test_ok");
        assert_eq!(f(py, "tests.test_calc::test_ok"), "test_ok");
        assert_eq!(
            f(py, "tests/test_calc.py::test_param[a b-c::d]"),
            "test_param"
        );
        assert_eq!(
            f(py, "tests.test_calc::test_param[x[1]-y]"),
            "test_param[x[1]-y]"
        );
    }

    /// Round 17: `{filter}` inside a double-quoted argument of a wrapper
    /// closed the quote, so the shell split the value, and the container's
    /// shell expanded its `$` again. It goes in single-quoted, unexpanded.
    #[test]
    fn a_filter_inside_a_wrapper_argument_arrives_as_one_quoted_word() {
        use super::*;
        let c: Config = toml::from_str(
            r#"
            [suite.py]
            run = "./dev \"pytest\""
            select = "./dev \"python3 -m pytest -v -k {filter}\""
            [suite.mvn]
            run = "x"
            select = "docker run --rm img sh -c 'mvn -Dtest={filter}' && echo {filter}"
            "#,
        )
        .unwrap();
        let spec = c.run_spec("py", Some("a b")).unwrap();
        assert_eq!(
            spec.command,
            CommandSpec::Shell("./dev \"python3 -m pytest -v -k $PROVA_FILTER_Q\"".into())
        );
        // Outside any quote, and inside single quotes, as before.
        let spec = c.run_spec("mvn", Some("a")).unwrap();
        assert_eq!(
            spec.command,
            CommandSpec::Shell(
                "docker run --rm img sh -c 'mvn -Dtest=\"$PROVA_FILTER\"' && echo \"$PROVA_FILTER\""
                    .into()
            )
        );
        assert_eq!(shell_single_quote("Calc$Div'x y"), r"'Calc$Div'\''x y'");
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
        assert_eq!(spec.stall_after_ms, 20_000);
        // Round 16: a recipe for a tool the select command does not run.
        assert!(super::recipe_applies("gotestsum-json", "make test"));
        assert!(!super::recipe_applies(
            "gotestsum-json",
            "go test -json -count=1 -run \"$PROVA_FILTER\" ./..."
        ));
        assert!(super::recipe_applies(
            "gotestsum-json",
            "gotestsum -- -run x ./..."
        ));
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
