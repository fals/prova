<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/prova-logo-dark.svg">
  <img alt="prova" src="docs/assets/prova-logo-light.svg" width="260">
</picture>

**prova: tests with memory, experiments with proof.**

[Getting started](docs/getting-started.md) •
[For agents](#for-agents) •
[Architecture](docs/architecture.md) •
[Vision](docs/vision.md)

</div>

prova runs your project's own test command, `make test`, `cargo test`, `pytest`,
anything, and turns it into fast, structured feedback. It reports the first
failure while the suite is still running, notices a hung test without a giant
timeout, remembers every run so a failure is known as new, old or flaky, and
decides performance questions with a frozen protocol and statistics instead
of one lucky benchmark. It is one binary, built for coding agents and the
people who run them.

```console
$ prova run --until first-failure -- make test
run 3fa9c1d2  suite cmd:38dcb90d  `make test`
FAILING (run still going)  1.2s  level recognized output  code tree:4e1f0c..+dirty:9a2b..
  FAIL calc::divides [new failure, last passed at tree:4e1f0c..]: expected 4, got 16
  FAIL calc::flaky_clock [failed 3x before, FLAKY]: clock tick landed on a bad boundary
next: fix and rerun, or `prova cancel 3fa9c1d2`; `prova wait 3fa9c1d2 --after 14` to keep watching
```

## Highlights

- **Zero config on any project.** `prova run -- <your test command>`. Detail
  rises automatically from exit code, to JUnit/TRX files, to live output, to
  native streams. ([fidelity ladder](docs/architecture.md#seeing-tests-the-fidelity-ladder))
- **No long timeouts.** Runs detach; `prova wait` returns within seconds on the
  first failure, a slow test (named) or a stall. ([liveness](docs/architecture.md#liveness-slow-stalled-killed))
- **Memory.** Every attempt is stored with the exact code it ran on: new
  failure, failed before, flaky, last green commit. ([history](docs/architecture.md#memory-history-and-flaky-tests))
- **Experiments, not anecdotes.** Any objective (time, steps, accuracy),
  read straight from `go test -bench`, criterion, pytest-benchmark, Vitest,
  JMH or BenchmarkDotNet. Pre-registered protocol, interleaved
  (ABBA) trials, Mann-Whitney test and confidence interval, accept/reject verdict.
  ([experiments](docs/architecture.md#experiments))
- **Specification first.** State the problem formally ([Quint](https://quint.sh),
  [TLA+](https://lamport.azurewebsites.net/tla/tla.html), [Lean](https://lean-lang.org), any checker), check it, then let it judge: a candidate that violates it is
  rejected before it is measured. ([skill](skills/prova-spec/SKILL.md))
- **Built for agents.** Skills for six languages, compact output with the next
  command, JSON output, stable exit codes, MCP tools. ([for agents](#for-agents))
- **One history per repository.** Clones and worktrees share it; every run
  records where it ran. ([storage](docs/architecture.md#storage))
- **Formally specified.** Run and experiment lifecycles are Quint specs
  (Quint is [TLA+](https://lamport.azurewebsites.net/tla/tla.html)'s logic
  in a typed, programmer-friendly syntax), model-checked and replayed against the Rust code. ([invariants](docs/architecture.md#invariants-and-where-they-are-enforced))

Rust, Go, Python, TypeScript, Java and C# are covered end to end by real
[fixture projects](fixtures/README.md).

## Install

Linux (x86_64, aarch64) and macOS (Apple silicon, Intel):

```sh
curl -fsSL https://raw.githubusercontent.com/fals/prova/main/install.sh | sh
```

With Docker: the image `ghcr.io/fals/prova` holds only the static binary
(amd64 and arm64). Copy it into the image where your tests run, since prova
runs your project's own test command:

```dockerfile
COPY --from=ghcr.io/fals/prova:v0.1.1 /prova /usr/local/bin/prova
```

Release tarballs with checksums are on the
[releases page](https://github.com/fals/prova/releases); from source:
`cargo install --git https://github.com/fals/prova prova-cli`. All options in
[Getting started](docs/getting-started.md#install).

## Quickstart

```sh
cd your-project
prova run -- make test                      # works right away, history starts now
prova init                                  # skills for agents, AGENTS.md section
prova adopt --write                         # proposes prova.toml from what it finds
prova run default --select test_name        # rerun one test
```

Is the change better?

```sh
prova exp metrics -- go test -run '^$' -bench . -count 3    # what prova can read
prova exp new speed --hypothesis "sieve beats trial division" \
  --metric BenchmarkCountPrimes.ns_per_op --min-effect 5% -- go test -run '^$' -bench . -count 3
# change the code, then
prova exp candidate speed && prova exp run speed
# ACCEPT  c1 vs baseline: BenchmarkCountPrimes.ns_per_op improved +94.9% (CI [+94.5%, +95.9%], p=0.0079)
```

More in [Getting started](docs/getting-started.md).

## For agents

Run `prova init` in a repository: it installs the workflow skills, plus the
skills of the languages it detects, where coding agents look for them, adds a
short section to `AGENTS.md`, and ignores prova's files. That reaches Claude
Code, Codex, GitHub Copilot, Cursor, Gemini CLI and Antigravity, OpenCode,
Amp, Kimi Code, Kimchi, pi, Goose, Devin (Windsurf), Cline, Roo, Zed,
Factory, Kiro, Junie and Warp ([what goes where](docs/getting-started.md#coding-agents)).
Then the agent knows the loop:

```text
prova run <suite> --until first-failure   first failure, slow test, stall or end (30 s)
prova run <suite> --until done            the result (9 min budget; `next:` continues)
prova run <suite> --select <t>    rerun what failed
prova history <t>                 new failure, pre-existing, or flaky?
prova exp ...                     any "is it better?" claim, gated by the spec
```

- **Exit codes:** 0 passed, 1 test failure, 2 usage, 3 stall/slow/killed,
  4 infrastructure, 5 still running.
- **JSON:** `run`, `wait`, `status`, `output`, `history`, `flaky`, `diff`,
  `sizes`, `adopt` and the `exp` reports take `--json`; events are JSON Lines
  with a [schema](schemas/event.v1.json).
- **MCP:** `claude mcp add prova -- prova mcp` (tools `run_start`, `run_wait`,
  `run_status`, `run_cancel`, `test_history`). The CLI is the primary
  interface: it is cheaper in context and works in every harness.
- **Skills:** `prova-adopt`, `prova-test`, `prova-experiment`, `prova-spec`, plus `prova-rust`,
  `prova-go`, `prova-python`, `prova-ts`, `prova-java`, `prova-csharp` in [`skills/`](skills/).
- **Machine-readable docs:** [`llms.txt`](llms.txt) and the
  [contracts](docs/architecture.md#contracts-for-agents).

Harness without skills support? Paste this into its instructions:

```markdown
Run tests only through prova: `prova run <suite> --until first-failure`
(returns within 30 s) or `--until done` for the result; continue with the
printed `next:` command, never a shell loop. Fix the first failure while the run
continues; rerun one test with `--select`. Check `prova history <test>` before
calling a failure new or flaky. Performance claims need `prova exp`.
```

## prova vs running tests directly

| | `cargo test` / `pytest` / `make test` in a shell | through prova |
|---|---|---|
| First failure | when the suite ends | while it runs |
| A hang | the harness timeout, minutes later | a stall event, seconds later |
| "Did I break this?" | guess | new / seen before / flaky / last green commit |
| What ran, with what | lost in scrollback | recorded: command, code, environment, origin |
| "It's faster now" | one run | protocol, trials, confidence interval, verdict |
| "It's correct" | the tests the agent just wrote | a specification fixed first, checked, used as the oracle |

## Why the name

*Prova* is Portuguese and Italian for test, trial and proof at once: run the
tests, run the trials, keep the proof. The mark is a lowercase p drawn with
nine dots, after the Portuguese *prova dos nove*, the conclusive check: eight
green runs and the red one the agent came for.

## Documentation

- [Getting started](docs/getting-started.md): install, adopt a project, the loop
- [Architecture](docs/architecture.md): components, diagrams, contracts, invariants
- [Vision](docs/vision.md): the problem prova was built to solve
- [Contributing](AGENTS.md): rules for humans and agents working on prova;
  prova is tested with prova

## Acknowledgements

prova's specification-first workflow stands on other people's work:

- [TLA+](https://lamport.azurewebsites.net/tla/tla.html), Leslie Lamport's
  language for specifying systems as state machines with invariants.
- [Quint](https://quint.sh) by Informal Systems: the same logic as
  TLA+ with a typed syntax close to code, a simulator and a test runner.
  prova's own lifecycles are written in it (`spec/`), and it is the default
  the `prova-spec` skill teaches.
- [Apalache](https://apalache-mc.org), the model checker behind
  `quint verify`.
- [Lean](https://lean-lang.org), for properties that need a proof; the
  executable-definition pattern follows
  [AWS Cedar](https://github.com/cedar-policy/cedar-spec).

Why these, and when to use which:
[Specification first](docs/getting-started.md#specification-first).

## License

[Apache-2.0](LICENSE)
