//! Skills shipped inside the binary (ADR 0012): always match this version.

/// (skill name, relative path, content)
pub const FILES: &[(&str, &str, &str)] = &[
    (
        "prova-adopt",
        "SKILL.md",
        include_str!("../../../skills/prova-adopt/SKILL.md"),
    ),
    (
        "prova-test",
        "SKILL.md",
        include_str!("../../../skills/prova-test/SKILL.md"),
    ),
    (
        "prova-experiment",
        "SKILL.md",
        include_str!("../../../skills/prova-experiment/SKILL.md"),
    ),
    (
        "prova-spec",
        "SKILL.md",
        include_str!("../../../skills/prova-spec/SKILL.md"),
    ),
    (
        "prova-spec",
        "references/conformance.md",
        include_str!("../../../skills/prova-spec/references/conformance.md"),
    ),
    (
        "prova-spec",
        "examples/ipam.qnt",
        include_str!("../../../skills/prova-spec/examples/ipam.qnt"),
    ),
    (
        "prova-spec",
        "examples/shortest.qnt",
        include_str!("../../../skills/prova-spec/examples/shortest.qnt"),
    ),
    (
        "prova-rust",
        "SKILL.md",
        include_str!("../../../skills/prova-rust/SKILL.md"),
    ),
    (
        "prova-rust",
        "references/pitfalls.md",
        include_str!("../../../skills/prova-rust/references/pitfalls.md"),
    ),
    (
        "prova-go",
        "SKILL.md",
        include_str!("../../../skills/prova-go/SKILL.md"),
    ),
    (
        "prova-python",
        "SKILL.md",
        include_str!("../../../skills/prova-python/SKILL.md"),
    ),
    (
        "prova-ts",
        "SKILL.md",
        include_str!("../../../skills/prova-ts/SKILL.md"),
    ),
    (
        "prova-java",
        "SKILL.md",
        include_str!("../../../skills/prova-java/SKILL.md"),
    ),
    (
        "prova-csharp",
        "SKILL.md",
        include_str!("../../../skills/prova-csharp/SKILL.md"),
    ),
];

/// Workflow skills are always installed; language skills when detected.
pub const WORKFLOW: &[&str] = &[
    "prova-adopt",
    "prova-test",
    "prova-experiment",
    "prova-spec",
];

pub const AGENTS_SECTION: &str = "<!-- prova:begin -->
## Tests and experiments: use prova

This project runs tests and performance experiments through `prova`.
Do not run test commands directly or wrap them in timeouts.

- Tests: `prova run <suite> --detach`, then `prova wait <run>`; fix the first
  failure while it runs. Skill: `prova-test`.
- Claims that a change is better (faster, fewer steps, more accurate): `prova exp ...` with a
  frozen protocol and a verdict. Skill: `prova-experiment`.
- Algorithms and protocols: write the specification first, check it, then
  implement and judge with it as the oracle. Skill: `prova-spec`.
- Suites are in `prova.toml`; no `prova.toml` yet? Start with skill
  `prova-adopt`. History lives in the prova home (`~/.local/share/prova`), shared
  by all clones and worktrees.
<!-- prova:end -->
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
