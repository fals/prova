# Getting started

## Install

One static binary, no daemon. Linux (x86_64, aarch64) and macOS (Apple
silicon, Intel).

### Install script

```sh
curl -fsSL https://raw.githubusercontent.com/fals/prova/main/install.sh | sh
```

It downloads the latest release for your OS and CPU, checks its SHA-256
checksum and installs `prova` into `~/.local/bin` (it says so if that
directory is not on your `PATH`). To pin a version or choose the directory,
set the variables on the `sh` side of the pipe:

```sh
curl -fsSL https://raw.githubusercontent.com/fals/prova/main/install.sh \
  | PROVA_VERSION=v0.1.1 PROVA_INSTALL_DIR=/usr/local/bin sh
```

The tarballs and their checksums are also attached to each
[GitHub release](https://github.com/fals/prova/releases).

### Docker

`ghcr.io/fals/prova` (tags `v0.1.1` and `latest`, amd64 and arm64) contains
only the static binary: no shell and no toolchain. prova runs your
project's own test command, so it must live where that toolchain is.
`docker run ghcr.io/fals/prova` can therefore only print its version; to
use it, copy the binary into the image where your tests run
(devcontainer, CI image, agent sandbox):

```dockerfile
FROM rust:1.95            # your project's own build image
COPY --from=ghcr.io/fals/prova:v0.1.1 /prova /usr/local/bin/prova
```

To take the binary out of the image without a Dockerfile:

```sh
id=$(docker create ghcr.io/fals/prova:v0.1.1)
docker cp "$id":/prova ~/.local/bin/prova
docker rm "$id"
```

Prefer a version tag over `latest`, so an image rebuild does not change
prova under you.

### From source

```sh
cargo install --git https://github.com/fals/prova --tag v0.1.1 prova-cli
```

### Platform notes

On macOS, prova watches test processes through `ps` instead of `/proc`.
Two checks need counters macOS does not give: a pass whose command used
no CPU at all is not flagged there, and experiments run without a
sandbox or CPU counters (every verdict says which checks did not run).

Experiments isolate each arm in a sandbox when
[bubblewrap](https://github.com/containers/bubblewrap) (`bwrap`) is
installed; without it they still run, and every verdict says the arms were
not isolated.

## Five minutes on an existing project

```
cd your-project
prova run -- make test          # any command; zero config; history starts now
prova init                      # skills for agents + AGENTS.md section + .gitignore
prova adopt --write             # proposes prova.toml from what it finds
prova adopt --verify default --filter <a real test name>
```

Then, in the fix loop (humans and agents alike):

```
prova run default --until first-failure   # first failure, slow test, stall, or end
prova run default --until done            # the result; `next:` continues if it takes longer
prova run default --select <test>
prova history <test>            # new failure, flaky, or pre-existing?
prova output <run>              # the kept excerpt; --test <name> for one failing test's output
prova run default --repeat 5    # is it flaky? each test's tally over 5 runs
```

A run whose results were not real (a broken fixture, a stale report) can be
taken out of every test's history: `prova forget <run> --reason "..."`.

Hidden detail behind the project's own command (`pytest -q`, Go's test
cache)? Opt in per suite, never by default:

```toml
[suite.make]
run = "make test"
inject = ["pytest-reportlog"] # every test live; or "pytest-junit", "go-nocache"
                              # also: prova run --inject <recipe> -- <command>
```

Every test live with its duration (native level): Rust via cargo-nextest's
libtest JSON (`prova adopt` proposes it when nextest 0.9.66+ is installed), Go via
`go test -json`, Python via `inject = ["pytest-reportlog"]`, Vitest via
`inject = ["vitest-stream"]` plus `--reporter=default --reporter="$PROVA_VITEST_REPORTER"`
in the suite command.

Exit codes: 0 passed, 1 test failure, 2 usage, 3 stall/slow/killed,
4 prova or infrastructure error, 5 still running.

## Is the change better?

Speed, step counts, accuracy: one procedure. See what a command yields, then
declare the experiment before measuring.

```
prova exp metrics -- go test -run '^$' -bench . -count 3       # BenchmarkX.ns_per_op, wall_ms, ...
prova exp new speed --hypothesis "sieve beats trial division" \
  --metric BenchmarkCountPrimes.ns_per_op --min-effect 5% -- go test -run '^$' -bench . -count 3
# change code (uncommitted; baseline is HEAD)
prova exp candidate speed
prova exp run speed             # ABBA trials, Mann-Whitney + Hodges-Lehmann CI, verdict
prova exp log speed             # what failed: a trial, the setup, the oracle
```

Native benchmark output is read directly (`go test -bench`, criterion,
pytest-benchmark, Vitest bench, JMH, BenchmarkDotNet); anything else prints
`{"metric": "accuracy", "value": 0.93}` lines. Use `--better higher` for
accuracy or throughput, `--setup "npm ci"` when the checkouts need
dependencies installed: it runs once in each variant's checkout (the
baseline's and the candidate's snapshots), with network.

By default a candidate may change anything but tests, benchmarks, build and
runner files. When the change needs one of those (a new dependency in
`go.mod`), allow it with `--change go.mod` (a glob, repeatable);
`--change-only` limits the candidate to those paths. Trials and the oracle
run without network (loopback only) when the arms are sandboxed; pass
`--allow-network` when the workload or the oracle needs it.

An ACCEPT makes the candidate the new best only if it beats the current
best. To go back after one that turned out wrong:
`prova exp best speed baseline --reason "..."`.

## Specification first

For algorithms and protocols, state what a correct answer is before writing
code, check that statement, and let it judge (skill `prova-spec`).

Why: an agent that writes the code, the tests and the success criterion in
one go can always declare success, and a faster wrong answer looks like a
win. A specification written and checked first is a judge the candidate
cannot edit: prova freezes it by content hash and rejects any candidate
that violates it before measuring anything.

prova runs any checker as the oracle; it embeds none. Which one to use:

| Tool | What it is | Use it when | Needs |
|---|---|---|---|
| [Quint](https://quint.sh) (Informal Systems) | [TLA+](https://lamport.azurewebsites.net/tla/tla.html)'s logic (Lamport's Temporal Logic of Actions: state, actions, invariants) in a typed syntax close to code, with a simulator, a test runner and traces to replay against the implementation | the default: state machines, protocols, allocators, reference answers | Node; `npm i -g @informalsystems/quint` (prova's CI uses 0.33.1) |
| TLA+ | the original language, with the TLC and [Apalache](https://apalache-mc.org) model checkers | the project already has TLA+ specifications | Java |
| [Lean](https://lean-lang.org) | a proof assistant: an executable definition plus theorems checked at build time | a property needs a proof for every input, not a bounded check | [elan](https://github.com/leanprover/elan); `lake build` is the oracle |
| A frozen labelled dataset | the expected output for each input | accuracy objectives (classification, ranking, extraction) | nothing |

Quint's simulator (`quint run`) samples behaviours at random; `quint verify`
checks exhaustively through Apalache and needs Java. prova's own run and
experiment lifecycles are Quint specifications in `spec/`, replayed against
the Rust engine ([ADR 0011](adr/0011-formalization-quint-not-lean.md),
[ADR 0015](adr/0015-spec-first-experiments.md)).

```
quint run spec/ipam.qnt --invariant safety        # the specification holds
prova run conformance                                # the implementation follows it
prova exp new ipam-fast --hypothesis "free list makes ADD O(1)" \
  --spec spec/ipam.qnt --oracle "make conformance" \
  --metric add_ns --min-effect 10% -- ./bench-add.sh
```

The specification is frozen with the protocol; a candidate that fails the
oracle is rejected without being measured.

## Coding agents

`prova init` puts the skills and instructions where each harness looks
([research, with sources](research/04-agent-harnesses.md)):

| Writes | Read by |
|---|---|
| `.agents/skills/` | Codex, GitHub Copilot, Cursor, Gemini CLI, Antigravity, OpenCode, Amp, Kimi Code, pi, Goose, Devin (Windsurf), Roo, Zed, Factory, Warp |
| `.claude/skills/` | Claude Code, Cline, Kimchi (and Copilot, Cursor, OpenCode, Amp, Goose) |
| `.kiro/skills/`, `.junie/skills/` | Kiro, Junie: only when `.kiro/` or `.junie/` already exists |
| a prova section in `AGENTS.md` | nearly every harness; it says where the skills are |
| the same section in `CLAUDE.md`, `GEMINI.md`, `.cursorrules`, `.rules`, `.windsurfrules`, `.clinerules`, `.github/copilot-instructions.md` | only where the file already exists: a `CLAUDE.md` makes Claude Code skip `AGENTS.md`, Gemini CLI reads `GEMINI.md`, Zed reads only the first of these it finds |

It creates none of those instruction files, so it never hides `AGENTS.md`
from another harness, and it writes no MCP configuration (the skills drive
the CLI). Aider reads nothing by itself: start it with
`aider --read AGENTS.md`. Running `prova init` again updates the section in
place.

## For MCP clients

```
prova mcp                       # stdio; tools run_start, run_wait, run_status, run_cancel, test_history
```

Example Claude Code config: `claude mcp add prova -- prova mcp`.

## In CI

prova needs no service in CI: keep its home in the job and persist it.

```yaml
# GitHub Actions
- uses: actions/cache@v4
  with:
    path: .prova-home
    key: prova-${{ github.ref_name }}-${{ github.run_id }}
    restore-keys: prova-${{ github.ref_name }}-
- run: prova run --json -- make test
  env:
    PROVA_HOME: ${{ github.workspace }}/.prova-home
    PROVA_ACTOR: github-actions
```

```yaml
# GitLab CI
test:
  variables:
    PROVA_HOME: $CI_PROJECT_DIR/.prova-home
  cache:
    key: prova-$CI_COMMIT_REF_SLUG
    paths: [.prova-home/]
  script:
    - prova run --json -- make test
```

Parallel jobs saving the same cache overwrite each other (last save wins), and
SQLite must not be shared by several writers over network storage (NFS,
ReadWriteMany volumes): give each job its own home, or one writer per volume.

## What prova stores

One central home per user: `~/.local/share/prova` (or `PROVA_HOME`),
with one SQLite file per repository plus a small catalog. Clones and
worktrees of the same repository (same first commit) share one history; the
workspace itself keeps nothing. `prova status --all` and `prova flaky --all`
look across every repository you work on. Every run records where it ran
(host, pod, node, actor: `PROVA_ACTOR`, CI job or user).

Commands and output are stored as given, with owner-only permissions:
treat the home like a log directory and do not share it. A store left in a
workspace `.prova/` by an older prova is imported on first use. If the home is
not writable, prova falls back to `<workspace>/.prova/` and says so.
