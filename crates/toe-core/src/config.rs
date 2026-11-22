//! `toe.toml` (optional, ADR 0013) and the run specification derived from
//! it or from a bare command.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{parse_duration, sha256_hex};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
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
    #[error("no suite named {0:?} in toe.toml (known: {1})")]
    UnknownSuite(String, String),
    #[error("suite {0} has no `select` template; cannot run a single test")]
    NoSelect(String),
    #[error(
        "invalid selection {0:?}: must be non-empty, not start with '-', no control characters"
    )]
    BadSelection(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    Small,
    Medium,
    Large,
}

impl Size {
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
}

/// How to start the process under test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandSpec {
    /// Executed directly, no shell.
    Argv(Vec<String>),
    /// Executed with `sh -c`; comes from `toe.toml`, owned by the project.
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
}

pub const DEFAULT_STALL_AFTER: Duration = Duration::from_secs(60);
pub const DEFAULT_KILL_AFTER: Duration = Duration::from_secs(3600);

/// Workspace root: nearest ancestor with `toe.toml`, else with `.git`,
/// else `start` itself.
pub fn find_root(start: &Path) -> PathBuf {
    for marker in ["toe.toml", ".git"] {
        if let Some(dir) = start.ancestors().find(|d| d.join(marker).exists()) {
            return dir.to_path_buf();
        }
    }
    start.to_path_buf()
}

pub fn load(root: &Path) -> Result<Config, ConfigError> {
    let path = root.join("toe.toml");
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
                // The filter is passed in TOE_FILTER and expanded by the shell
                // as one quoted word; its text never becomes shell syntax.
                s.select
                    .as_ref()
                    .ok_or_else(|| ConfigError::NoSelect(name.into()))?
                    .replace("{filter}", "\"$TOE_FILTER\"")
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
        })
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
            CommandSpec::Shell("make test TEST=\"$TOE_FILTER\"".into())
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
