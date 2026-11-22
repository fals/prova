# AGENTS.md

Guide for every agent, human or model, working on toe. Read it fully before changing anything.

## What toe is

toe (Test Oriented Experimentation) gives coding agents one structured way to run tests and experiments on a system under test, get fast event-driven feedback, and keep a history of what ran, with which inputs, and what happened. Vision: [README.md](README.md). Decisions: [docs/adr/](docs/adr/README.md). Evidence: [docs/research/](docs/research/).

toe is built with toe. As soon as a usable version exists, every test run and every performance claim in this repository goes through it (see [Dogfooding](#dogfooding)).

## Current state

- Phase: M0 to M7 built on stacked branches (PRs #37 to #44, merge in order). **Dogfooding Stage B is active**: run tests through toe.
- Open: ADRs still Proposed; GitHub Actions blocked by account billing (`scripts/ci.sh` is the gate).
- ADRs 0001 to 0013 are **Proposed**. Open points awaiting the owner's decision:
  - 0009 conduit (CLI primary, MCP facade), 0010 deployment (static binary, container as packaging), 0011 formalization (Quint, Lean deferred).
  - 0013 black-box adoption: environment injection opt-in, PATH shims excluded.
- [0013](docs/adr/0013-black-box-adoption.md) amends 0001, 0003, 0004, 0005 and 0012: `toe run -- <existing command>` works with zero config, fidelity rises from opaque process to result files to output recognizers to native streams, and test size is observed before it is declared. Where they conflict, 0013 wins.
- Work is tracked as GitHub milestones M0 to M7 with issues.
- Until an ADR is Accepted, build only what the milestone in progress needs, and flag any code that depends on an unresolved point.

## Ground rules

1. **ADRs are the source of truth.** Code that contradicts an Accepted ADR is a bug. To change a decision, write a new ADR that supersedes the old one; never rewrite an Accepted ADR in substance.
2. **Spec before engine.** Any change to the run or experiment lifecycle starts in `spec/` (Quint), passes the model checker, then lands in Rust ([0011](docs/adr/0011-formalization-quint-not-lean.md)).
3. **Black box first.** toe must work on an existing test command with no changes to the user's project. Never make a feature depend on the user editing their scripts.
4. **No long waits.** Nothing in toe, its tests, or your own workflow blocks on a long timeout. Detach, then wait for events with a short maximum ([0004](docs/adr/0004-liveness-and-fail-fast.md)).
5. **Simplest complete thing.** No speculative config, plugin points, or services. One binary, one SQLite file, no daemon ([0001](docs/adr/0001-architecture-overview.md)).
6. **Never lose data.** Raw output is always kept as an artifact; schema changes always go through migrations.

## Repository layout (target)

```
crates/
  toe-core/       model, engine, liveness, store, analysis, statistics
  toe-adapters/   framework adapters and output recognizers
  toe-cli/        the `toe` binary, including `toe mcp`
spec/             Quint specs: run.qnt, experiment.qnt
skills/           Agent Skills shipped with the binary (toe-test, toe-adopt, toe-rust, ...)
fixtures/         sample projects per language, used by integration and large tests
docs/adr/         decisions
docs/research/    sourced research
toe.toml          toe's own suites, once dogfooding starts
```

Create directories when the first file needs them, not before.

## Build and test

Rust stable, edition 2024, Cargo workspace.

```
cargo build
cargo install --path crates/toe-cli --root .toe/stable   # refresh stable toe from green main
.toe/stable/bin/toe run small                             # inner loop
scripts/ci.sh                                             # full local gate (mirror of CI)
```

Test commands depend on the dogfooding stage, below.

## Dogfooding

**Stage A, before M1 is done:** run tests with `cargo nextest run` (or `cargo test`). Run small tests first, use fail-fast, never wrap them in a long timeout.

**Stage B, from M1 on:** every test run goes through toe.

```
toe run small            # inner loop
toe wait <run> --until first-failure --max-wait 30s
toe run medium           # before opening a PR
toe history <test>       # before calling a failure new or flaky
```

- Use the **stable** toe binary to test the **development** toe. The stable binary is built from the last green `main` and installed with `cargo install --path crates/toe-cli --root .toe/stable` (path ignored by git). Never test toe with the binary you are changing: a bug would hide itself.
- Never run raw test commands in Stage B, except to debug toe itself, and say so in your report. A Claude Code hook (`.claude/hooks/require-toe.sh`) blocks direct `cargo test` / `cargo nextest`; other harnesses must follow the rule by hand.
- Every performance claim (faster, smaller, fewer allocations) requires a toe experiment with a frozen protocol and an `accept` verdict ([0007](docs/adr/0007-experiments.md)). Link the experiment id in the PR. "It looked faster once" is not evidence.
- If toe gets in the way, that is a toe bug: open an issue labelled `dogfood` with the exact command and run id, then work around it.

## Testing rules

Sizes follow [0005](docs/adr/0005-test-sizes.md).

| Size | Allowed | Where |
|---|---|---|
| small | one process, no disk, no network, no sleep | unit tests next to the code |
| medium | subprocesses, temp dirs, SQLite file, localhost | `crates/*/tests/` |
| large | real fixture projects, real toolchains, containers | `tests/large/`, opt-in |

- Write the smallest test that can fail for the right reason. A medium or large test needs one line explaining why a smaller one cannot do it.
- No `sleep` to wait for something to happen. Wait on an event or a condition with a bound.
- Every bug fix starts with a failing test.
- Lifecycle changes: spec, then trace replay test, then code.
- Statistics: verify against reference values computed with R or SciPy, stored as test vectors with the command that produced them.
- Process handling (signals, process groups, leaked children) is tested with real processes in medium tests, on Linux in CI.

## Multi-agent development

Work is split across specialized agents until toe is usable. One agent owns each area; others review.

| Area | Owner | Reviewer |
|---|---|---|
| Quint specs, lifecycle invariants, trace replay | distributed-systems-engineer | rust-engineer |
| toe-core engine, store, analysis, statistics | rust-engineer | distributed-systems-engineer |
| Process supervision, liveness, signals, process-tree observation | linux-plumber | rust-engineer |
| Adapters and recognizers (Rust, Go) | rust-engineer, golang-engineer | owner of toe-core |
| Adapters and recognizers (Python, TS, Java, C#) | general-purpose | rust-engineer |
| Skills | general-purpose, validated by an agent using them on a fixture | owner |
| Container image, release, CI | kubernetes-expert | linux-plumber |
| Security (command execution, artifact paths, MCP facade) | security-pentester | owner of the area |

Rules for agents:

1. **One task, one branch, one PR.** Branch name `<area>/<short-name>`. Do not touch areas you do not own without the owner reviewing.
2. **Start from the issue.** Every task maps to a GitHub issue under a milestone. State in the PR which ADR and which acceptance criteria it covers.
3. **Contracts first when crossing areas.** If your change alters an event, a CLI flag, a table, or a spec, update the contract (Rust type plus JSON Schema, spec, or ADR) in its own commit before the code that uses it.
4. **Hand off with evidence.** A handoff states what was done, the toe run ids (Stage B) or test output (Stage A) that prove it, and what is known to be missing. No claim without a run id or output.
5. **Do not trust a summary, including your own.** Before building on another agent's work, run its tests.
6. **Parallel work needs disjoint files.** Coordinate on shared files (`Cargo.toml`, migrations, schema) through the owner of toe-core.
7. **Escalate, do not guess**, on: anything that contradicts an ADR, a public CLI or event format change, a security trade-off, or a new dependency with a large footprint.

## Milestones until usable

Each milestone ends with something an agent can actually use.

| | Milestone | Done when |
|---|---|---|
| M0 | Skeleton | workspace builds; CI runs fmt, clippy, tests, Quint check; `spec/run.qnt` model-checked |
| M1 | Opaque run | `toe run -- <cmd>`, `--detach`, `wait`, `cancel`, `status`; process-group kill; stall from process-tree activity; runs and artifacts in SQLite. **Switch to Stage B.** |
| M2 | Result files | JUnit XML found and ingested after a run; per-test outcomes, identity, history, flake statistics, failure fingerprints |
| M3 | Live tests | nextest adapter (toe tests itself at full fidelity), Go adapter, output recognizers; first-failure event mid-run |
| M4 | Experiments | `toe exp` with frozen protocol, interleaved trials, Mann-Whitney plus effect interval, verdicts; `spec/experiment.qnt` |
| M5 | Adoption | `toe init`, skills `toe-adopt`, `toe-test`, `toe-rust`; an agent adopts a fixture project with no human help |
| M6 | Reach | MCP facade; pytest and Vitest adapters; remaining language skills |
| M7 | Release | static binaries, container image, install docs |

**Usable** means: an agent with no prior knowledge of toe, given only the skills, adopts an existing project whose tests run via `make test`, runs the fix loop, and gets the first failure without waiting for the suite to end. This is checked on the fixtures by an agent session, with the run recorded in toe.

## Code standards

- `thiserror` for library errors, `anyhow` only in the binary. No `unwrap` or `expect` outside tests, except with a comment proving it cannot fail.
- No `unsafe` without an ADR-level reason and a comment.
- Public types that cross a boundary (events, CLI JSON output, stored payloads) derive `serde` and `schemars`; the generated schemas are committed and checked in CI.
- Every new dependency is justified in the PR description: what it replaces, its size, its maintenance status.
- Output for agents is compact: failures first, counts for passes, details by id on request.
- Exit codes are a contract ([0004](docs/adr/0004-liveness-and-fail-fast.md)); changing one is a breaking change.

## Git

- Default branch `main`, protected by convention: changes arrive through PRs, except docs-only commits the owner asks for directly.
- GitHub Actions cannot currently start jobs on this account (billing). Until fixed, `scripts/ci.sh` is the merge gate; paste its last line in the PR.
- Commit messages: imperative subject under 72 characters, body explaining why.
- Model-authored commits end with the `Co-Authored-By` line given by the harness.
- Never commit `.toe/`, `target/`, secrets, or machine-specific paths.

## When in doubt

Check the ADRs, then the research, then ask the owner. Prefer a short question over a wrong week.
