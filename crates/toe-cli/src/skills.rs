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
    (
        "toe-go",
        "SKILL.md",
        include_str!("../../../skills/toe-go/SKILL.md"),
    ),
    (
        "toe-python",
        "SKILL.md",
        include_str!("../../../skills/toe-python/SKILL.md"),
    ),
    (
        "toe-ts",
        "SKILL.md",
        include_str!("../../../skills/toe-ts/SKILL.md"),
    ),
    (
        "toe-java",
        "SKILL.md",
        include_str!("../../../skills/toe-java/SKILL.md"),
    ),
    (
        "toe-csharp",
        "SKILL.md",
        include_str!("../../../skills/toe-csharp/SKILL.md"),
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
- Suites are in `toe.toml`; no `toe.toml` yet? Start with skill
  `toe-adopt`. History lives in the toe home (`~/.local/share/toe`), shared
  by all clones and worktrees.
<!-- toe:end -->
";

#[cfg(test)]
mod tests {
    /// Every file under `skills/` must be embedded, and nothing else.
    #[test]
    fn all_skill_files_are_embedded() {
        fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, base, out);
                } else {
                    out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
                }
            }
        }
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
        let mut on_disk = Vec::new();
        walk(&base, &base, &mut on_disk);
        on_disk.sort();
        let mut embedded: Vec<String> = super::FILES
            .iter()
            .map(|(s, r, _)| format!("{s}/{r}"))
            .collect();
        embedded.sort();
        assert_eq!(embedded, on_disk);
    }
}
