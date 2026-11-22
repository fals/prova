<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/logo-dark.svg">
  <img alt="toe" src="docs/assets/logo-light.svg" width="260">
</picture>

**Test oriented experimentation: tests and experiments that coding agents can trust.**

[Getting started](docs/getting-started.md) •
[For agents](#for-agents) •
[Architecture](docs/architecture.md) •
[Decisions](docs/adr/README.md) •
[Vision](docs/vision.md)

</div>

toe runs your project's own test command, `make test`, `cargo test`, `pytest`,
anything, and turns it into fast, structured feedback. It reports the first
failure while the suite is still running, notices a hung test without a giant
timeout, remembers every run so a failure is known as new, old or flaky, and
decides performance questions with a frozen protocol and statistics instead
of one lucky benchmark. It is one binary, built for coding agents and the
people who run them.

```console
$ toe run --detach -- make test
run 3fa9c1d2 started
next: toe wait 3fa9c1d2

$ toe wait 3fa9c1d2
run 3fa9c1d2  suite cmd:38dcb90d  `make test`
FAILING (run still going)  1.2s  level recognized output  code tree:4e1f0c..+dirty:9a2b..
  FAIL calc::divides [new failure, last passed at tree:4e1f0c..]: expected 4, got 16
  FAIL calc::flaky_clock [seen 3x before, FLAKY]: clock tick landed on a bad boundary
next: fix and rerun, or `toe cancel 3fa9c1d2`
```

## Highlights

- **Zero config on any project.** `toe run -- <your test command>`. Detail
  rises automatically from exit code, to JUnit/TRX files, to live output, to
  native streams. ([fidelity ladder](docs/architecture.md#seeing-tests-the-fidelity-ladder))
- **No long timeouts.** Runs detach; `toe wait` returns within seconds on the
  first failure, a slow test (named) or a stall. ([liveness](docs/architecture.md#liveness-slow-stalled-killed))
- **Memory.** Every attempt is stored with the exact code it ran on: new
  failure, seen before, flaky, last green commit. ([history](docs/architecture.md#memory-history-and-flaky-tests))
- **Experiments, not anecdotes.** Pre-registered protocol, interleaved trials,
  Mann-Whitney test and confidence interval, accept/reject verdict, improvement
  loops. ([experiments](docs/architecture.md#experiments))
- **Built for agents.** Skills for six languages, compact output with the next
  command, JSON everywhere, stable exit codes, MCP tools. ([for agents](#for-agents))
- **One history per repository.** Clones and worktrees share it; every run
  records where it ran. ([storage](docs/architecture.md#storage))
- **Formally specified.** Run and experiment lifecycles are Quint specs,
  model-checked and replayed against the Rust code. ([invariants](docs/architecture.md#invariants-and-where-they-are-enforced))

Rust, Go, Python, TypeScript, Java and C# are covered end to end by real
[fixture projects](fixtures/README.md).

## Install

```sh
cargo install --git https://github.com/fals/toe toe-cli
```

In a container image, copy the static binary next to your toolchain:

```dockerfile
COPY --from=ghcr.io/fals/toe:latest /toe /usr/local/bin/toe
```

Release binaries and the image are produced by the release workflow
(Linux musl x86_64/aarch64, macOS) once releases are published.

## Quickstart

```sh
cd your-project
toe run -- make test                      # works right away, history starts now
toe init                                  # skills for agents, AGENTS.md section
toe adopt --write                         # proposes toe.toml from what it finds
toe run default --select test_name        # rerun one test
```

Performance question?

```sh
toe exp new speed --hypothesis "cache cuts latency" --metric wall_ms --min-effect 2% -- ./bench.sh
# change the code, then
toe exp candidate speed && toe exp run speed
```

More in [Getting started](docs/getting-started.md).

## For agents

Run `toe init` in a repository: it installs the skills into `.agents/skills`
(and `.claude/skills`), adds a short section to `AGENTS.md`, and ignores
toe's files. Then the agent knows the loop:

```text
toe run <suite> --detach        start, get a run id
toe wait <run>                  first failure, slow test, stall or end (max 30 s)
toe run <suite> --select <t>    rerun what failed
toe history <t>                 new failure, pre-existing, or flaky?
toe exp ...                     any "is it faster?" claim
```

- **Exit codes:** 0 passed, 1 test failure, 2 usage, 3 stall/slow/killed,
  4 infrastructure, 5 still running.
- **JSON:** every command takes `--json`; events are JSON Lines with a
  [schema](schemas/event.v1.json).
- **MCP:** `claude mcp add toe -- toe mcp` (tools `run_start`, `run_wait`,
  `run_status`, `run_cancel`, `test_history`). The CLI is the primary
  interface: it is cheaper in context and works in every harness.
- **Skills:** `toe-adopt`, `toe-test`, `toe-experiment`, plus `toe-rust`,
  `toe-go`, `toe-python`, `toe-ts`, `toe-java`, `toe-csharp` in [`skills/`](skills/).
- **Machine-readable docs:** [`llms.txt`](llms.txt) and the
  [contracts](docs/architecture.md#contracts-for-agents).

Harness without skills support? Paste this into its instructions:

```markdown
Run tests only through toe. Start with `toe run <suite> --detach`, then
`toe wait <run>` (returns within 30 s). Fix the first failure while the run
continues; rerun one test with `--select`. Check `toe history <test>` before
calling a failure new or flaky. Performance claims need `toe exp`.
```

## toe vs running tests directly

| | `cargo test` / `pytest` / `make test` in a shell | through toe |
|---|---|---|
| First failure | when the suite ends | while it runs |
| A hang | the harness timeout, minutes later | a stall event, seconds later |
| "Did I break this?" | guess | new / seen before / flaky / last green commit |
| What ran, with what | lost in scrollback | recorded: command, code, environment, origin |
| "It's faster now" | one run | protocol, trials, confidence interval, verdict |

## Documentation

- [Getting started](docs/getting-started.md): install, adopt a project, the loop
- [Architecture](docs/architecture.md): components, diagrams, contracts, invariants
- [Decisions (ADRs)](docs/adr/README.md) and [research](docs/research/)
- [Vision](docs/vision.md): the problem toe was built to solve
- [Contributing](AGENTS.md): rules for humans and agents working on toe;
  toe is tested with toe

## License

Apache-2.0 (as declared in `Cargo.toml`).
