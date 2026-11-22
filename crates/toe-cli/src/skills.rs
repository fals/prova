//! Skills shipped inside the binary (ADR 0012): always match this version.

/// (skill name, relative path, content)
pub const FILES: &[(&str, &str, &str)] = &[
    (
        "toe-adopt",
        "SKILL.md",
        include_str!("../../../skills/toe-adopt/SKILL.md"),
    ),
    (
        "toe-test",
        "SKILL.md",
        include_str!("../../../skills/toe-test/SKILL.md"),
    ),
    (
        "toe-experiment",
        "SKILL.md",
        include_str!("../../../skills/toe-experiment/SKILL.md"),
    ),
    (
        "toe-rust",
        "SKILL.md",
        include_str!("../../../skills/toe-rust/SKILL.md"),
    ),
    (
        "toe-rust",
        "references/pitfalls.md",
        include_str!("../../../skills/toe-rust/references/pitfalls.md"),
    ),
];

/// Workflow skills are always installed; language skills when detected.
pub const WORKFLOW: &[&str] = &["toe-adopt", "toe-test", "toe-experiment"];

pub const AGENTS_SECTION: &str = "<!-- toe:begin -->
## Tests and experiments: use toe

This project runs tests and performance experiments through `toe`.
Do not run test commands directly or wrap them in timeouts.

- Tests: `toe run <suite> --detach`, then `toe wait <run>`; fix the first
  failure while it runs. Skill: `toe-test`.
- Performance claims: `toe exp ...` with a frozen protocol and a verdict.
  Skill: `toe-experiment`.
- Suites are in `toe.toml`; history lives in `.toe/` (not committed).
<!-- toe:end -->
";
