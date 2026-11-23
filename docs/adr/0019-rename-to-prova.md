# 0019: Rename toe to prova

Status: Accepted (2026-11-23). Amends the names used in
[0001](0001-architecture-overview.md), [0008](0008-storage-sqlite.md),
[0010](0010-deployment.md), [0012](0012-skills-packaging.md) and
[0014](0014-central-store-and-server.md); no behaviour changes. Proposed as
0018 on a branch; renumbered 0019 because 0018 was taken by the honest-history
record merged in the meantime.

## Context

The project is called toe: the harness and the model kept stubbing their
little toe on tests (no memory of earlier runs, `cat`-a-file scripts, absurd
timeouts), and "Test Oriented Experimentation" was fitted to the word
afterwards. The name is short and has a story, but:

- it names the injury, not what the tool gives: evidence about tests and
  experiments;
- it is hard to search for ("toe" is a body part and several acronyms);
- the `toe` crate on crates.io belongs to someone else (checked 2026-11-23),
  so `cargo install toe` can never be ours;
- it gives no natural mark; the current flask logo does not say "toe".

Nothing has been released: the workspace is at 0.1.0 and the repository has
no tags. The only users are this repository's own dogfooding and the
fixtures. A rename will never be cheaper than now.

## Decision

The project, the binary and everything named after it become **prova**.
"Prova" is Portuguese and Italian for test, trial and proof at once, which is
the product: run the tests, run the trials, keep the proof. There is no
backronym.

### What is renamed

| Today | Becomes |
|---|---|
| binary `toe` (`toe run`, `toe exp`, `toe mcp`, ...) | `prova` |
| crates `toe-core`, `toe-adapters`, `toe-cli` | `prova-core`, `prova-adapters`, `prova-cli` |
| config `toe.toml` | `prova.toml` |
| workspace directory `.toe/` (and the `.gitignore` line `toe init` adds) | `.prova/` |
| `TOE_HOME`, default `$XDG_DATA_HOME/toe` or `~/.local/share/toe` | `PROVA_HOME`, `$XDG_DATA_HOME/prova` or `~/.local/share/prova` |
| per-repository store `toe.db` | `prova.db` |
| every other `TOE_*` variable (`TOE_RUN_ID`, `TOE_FILTER`, `TOE_PARAM_*`, `TOE_ACTOR`, `TOE_STREAM`, `TOE_TRACE_DIR`, `TOE_VITEST_REPORTER`, ...) | `PROVA_*` |
| skills `toe-test`, `toe-adopt`, `toe-experiment`, `toe-spec`, `toe-<language>` | `prova-*` |
| MCP server name and tool prefixes | `prova` |
| `toe-vitest-reporter.mjs` | `prova-vitest-reporter.mjs` |
| image `ghcr.io/fals/toe` | `ghcr.io/fals/prova` |
| repository `fals/toe` | `fals/prova` (renamed by the owner; GitHub redirects the old URL) |
| `.claude/hooks/require-toe.sh`, `.toe/stable` in AGENTS.md | `require-prova.sh`, `.prova/stable` |

Subcommands, flags, exit codes, the event format, the SQLite schema and the
specs do not change. Event and JSON field names stay as they are; if one
turns out to contain the old name, stop and ask the owner before changing it.

### What is kept, what is not

1. **History is kept.** On first use, if the prova home does not exist and
   the old toe home does, it is moved once and each `toe.db` (with its `-wal`
   and `-shm`) is renamed to `prova.db`. A workspace `.toe/` is moved to
   `.prova/` the same way. This follows the existing one-time move of
   `<root>/.toe/toe.db` into the central home (0014).
2. **Old config is detected, not read.** If `prova.toml` is absent and
   `toe.toml` is present, prova stops with one line telling the user to
   rename the file. It does not silently read the old name.
3. **No aliases.** No `toe` binary or symlink, no `TOE_*` fallback, no old
   skill names. Nothing was released, so there is nobody to keep working, and
   two names for everything would be permanent weight (ground rule 5).
4. **Accepted records are not rewritten.** ADRs 0001 to 0018 and
   `docs/research/` keep the word "toe"; `docs/adr/README.md` gets one line
   saying that toe is the former name of prova. README, AGENTS.md, `llms.txt`,
   `docs/*.md`, skills and code comments are updated.

### Identity

The mark is a lowercase **p drawn with nine dots** ("prova dos nove", the
Portuguese idiom for the conclusive check): eight green dots, one red. Each
dot is a remembered run; the red one is the failure the agent came for. Green
and red are the unit-testing pass and fail colours and are used for nothing
else: `#3DDC84` and `#FF5D6C` on the dark tile `#1B1530`.

The assets are in `docs/assets/` next to the old ones: `prova-mark.svg`,
`prova-logo-light.svg`, `prova-logo-dark.svg`, `prova-icon-512.png` and
`prova-social-preview.png`. The wordmark is drawn as paths, so it needs no
font. The rename PR points the README at them and deletes the toe flask
assets (`logo-*.svg`, `icon-512.png`, `social-preview.png`).

### How the rename lands

One branch, one PR, owned by the toe-core owner because it touches every
shared file (rule 6: no other branch should be open against the same files
when it merges).

1. Contracts first, in their own commit: the README index, regenerated `schemas/` if any description changes.
2. The rename itself, mechanical, with no behaviour change mixed in.
3. The migration in point 1 above, starting from a failing medium test that
   builds an old-layout home in a temp dir.
4. A test that `toe.toml` without `prova.toml` gives the rename hint.
5. The PR is tested with the stable **toe** binary from the last green main
   (the binary under change never tests itself). After merge, stable is
   reinstalled as `.prova/stable/bin/prova` and Stage B continues with
   `prova run small`.

## Consequences

- `cargo install prova-cli` and the crate names `prova`, `prova-core`,
  `prova-cli`, `prova-adapters` were free on crates.io on 2026-11-23. They
  should be reserved before the name is announced.
- Not checked: domains, npm, Homebrew, container registries other than GHCR,
  trademarks. The owner should check these before the first release.
- Every open branch conflicts with the rename. Merge or rebase them first.
- Run history from before the rename stays readable through the migration;
  run ids do not change.
- The stubbed-toe story moves from the name to the README's "why".
- English speakers do not get the meaning for free; the tagline has to carry
  it ("prova: tests with memory, experiments with proof").

## Alternatives

- **Keep toe.** Cheapest, keeps the story. Rejected by the owner: the crate
  name is taken, the word is unsearchable, and it names the pain.
- **tiptoe, assay, litmus, tally.** All taken on crates.io; each covers only
  part of the product (care, measurement, verdict, counting).
- **Rename with aliases** (`toe` symlink, `TOE_*` fallback, reading
  `toe.toml`). Rejected: there are no released users to protect, and aliases
  never get removed.
- **Rewrite the old ADRs.** Rejected: accepted records are not edited, and
  the old name is part of why they read the way they do.
