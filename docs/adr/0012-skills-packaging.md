# 0012: Skills: Agent Skills standard, workflow plus language skills

Status: Accepted (2026-11-22). Amended by [0013](0013-black-box-adoption.md): `prova-adopt` is the first skill built.

## Context

Skills are how the agent meets prova (`diagram.svg`). They must teach two things: how to use prova (the loops), and how to wire prova into a project in a given language. Agent Skills (`SKILL.md` plus optional `references/` and `scripts/`) is an open standard supported by the major harnesses; `.agents/skills` is the nearest common install path ([02](../research/02-conduit-skills-deployment.md)).

## Decision

**Two kinds of skill, kept separate.**

Workflow skills (language-neutral):

- `prova-test`: the test loop. Run small suites, wait for first failure, read the failure with its history, fix, rerun the failing selection, then widen. Rules for test size and for never running raw test commands.
- `prova-experiment`: write hypothesis and protocol, record baseline, run trials, read verdict.
- `prova-improve`: improvement loop on top of experiments.

Language skills (short, details in `references/`):

- `prova-rust`, `prova-go`, `prova-python`, `prova-ts`, `prova-java`, `prova-csharp`: which adapter, how to enable the framework's streaming output, the `prova.toml` suite entries, how to tag test sizes natively, common pitfalls.

**Rules.**

1. Standard-only frontmatter (`name`, `description`) so skills work in every harness.
2. Skills contain no logic that the binary should own. If a skill needs a script, that is a missing `prova` subcommand.
3. `SKILL.md` stays short; depth goes to `references/`, loaded on demand.
4. Skills are versioned with the binary and embedded in it: `prova init` writes `prova.toml`, installs the workflow skills plus the detected language skills into `.agents/skills` (and `.claude/skills` when that harness is detected), and adds a short section to `AGENTS.md`.
5. The repository also acts as a plugin marketplace for harnesses that have one.

**Order of work.** `prova-test` and one language skill first, validated on real projects, before the others are written.

## Consequences

- One source of truth for skills, always matching the installed binary.
- Adding a language is one adapter plus one short skill.
- Skill quality needs its own evaluation: scripted agent sessions on sample projects, measured by prova itself.

## Alternatives

- One large skill: loads irrelevant language detail into context.
- Skills only, no binary (pure instructions): no enforcement, no memory; the status quo with better manners.
