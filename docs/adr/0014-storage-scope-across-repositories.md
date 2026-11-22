# 0014: Storage scope across repositories and worktrees

Status: Proposed. Refines [0008](0008-storage-sqlite.md) and [0001](0001-architecture-overview.md).

## Context

toe has no daemon ([0001](0001-architecture-overview.md)): the binary is stateless and each workspace keeps its own store at `<root>/.toe/toe.db`, where the root is the nearest ancestor with `toe.toml`, else `.git`. Repositories are therefore separated by location on disk.

An engineer (or an agent fleet) maintaining many repositories exposes four gaps:

1. **Git worktrees split history.** Each worktree has its own `.git` file, so each becomes a separate root with its own store. Agents work in worktrees constantly (one per task); the same repository's runs, flake statistics and experiments end up scattered and invisible to each other.
2. **No view across repositories.** Nothing answers "what is failing or flaky anywhere I work".
3. **A store does not know which repository it belongs to.** After a move or a fresh clone, history cannot be linked back.
4. **Nested `toe.toml` in a mono-repo** start separate stores. That is the right behaviour for independent sub-projects but is not written down.

## Decision

### 1. One store per repository, shared by its worktrees

The store lives with the repository it describes, not in a central place:

- Inside git, the store directory is `<git common dir>/toe/` (for example `.git/toe/toe.db`), found with `git rev-parse --git-common-dir`. All worktrees of a clone resolve to the same common directory, so they share runs, tests, flake statistics and experiments.
- Outside git, the store stays at `<root>/.toe/` as today.
- Per-run files (output, injected reports, artifacts) move with it: `<store dir>/runs/`, `<store dir>/artifacts/`.
- The workspace root used to run commands is unchanged (the worktree itself); only where history is kept moves.
- Every run already records its code identity and working directory ([0006](0006-identity-history-flakiness.md)); runs from different worktrees stay distinguishable, and "same code" comparisons keep working across worktrees, which is the point.

Why the git directory: it is per clone, shared by worktrees, never committed, deleted with the clone, and outside the working tree, so tools that walk the workspace never see it.

### 2. Repository identity inside the store

A `meta` table records the repository identity when the store is created and on every open:

- `root_commit`: hash of the first commit (`git rev-list --max-parents=0 HEAD`, the oldest if several), stable across clones and moves;
- `remote`: the `origin` URL with credentials stripped;
- `last_root`: the last absolute path the store was opened from.

The identity lets the index (below) recognise the same repository after a move, and keeps history honest when a store is copied.

### 3. Optional per-user index for cross-repository views

A small file at `$XDG_STATE_HOME/toe/workspaces.json` (default `~/.local/state/toe/`) lists known stores: path, repository identity, last seen. toe adds an entry when it opens a store; it holds no test data.

- `toe status --all`, `toe flaky --all` and `toe history --all <test>` read each listed store read-only and merge the results, labelled by repository.
- Missing paths are skipped and pruned by `toe gc --index`.
- Setting `TOE_INDEX=off` disables the index entirely (CI, sandboxes).

### 4. Mono-repos

A nested `toe.toml` marks an independent sub-project only when it sets `[workspace] separate = true`; otherwise suites from nested `toe.toml` files belong to the enclosing repository's store, with the sub-project path recorded on each run. Default: one store per repository.

### 5. Migration

On first open by a version implementing this ADR, an existing `<root>/.toe/toe.db` inside a git repository is moved to the common git directory (with its `runs/` and `artifacts/`), and `<root>/.toe/` is left with a one-line `MOVED` note. If both exist (two worktrees with history), the older store is kept and the other's runs are imported; conflicts are impossible because run ids are random and test ids are content hashes.

## Consequences

- One history per repository regardless of how many worktrees or agents work in it; flaky detection gets more data, not less.
- Deleting a clone deletes its history; nothing accumulates in a central place.
- Concurrency stays per repository: parallel agents in worktrees of the same repository now share one SQLite file. WAL with short `BEGIN IMMEDIATE` transactions ([0008](0008-storage-sqlite.md)) is designed for this; the M1 medium tests cover two writers, and a test with several concurrent runs from different worktrees is added.
- Containers that mount only a worktree (not the common git directory) cannot reach the shared store; toe falls back to `<root>/.toe/` and says so in a run note.
- Cross-repository views cost one read per store; fine for tens of repositories.

## Alternatives

- **One central database for all repositories** (`~/.local/state/toe/toe.db`): simplest cross-repository queries, but mixes histories, makes deleting a repository's data a manual step, puts every agent on one write lock, and breaks in containers that only see the workspace. Rejected.
- **Keep per-worktree stores**: no change, but agents in worktrees lose all history between tasks. Rejected.
- **A daemon owning all stores**: solves cross-repository views and concurrency, at the cost of a service to run, which [0001](0001-architecture-overview.md) avoids. Revisit only if a shared dashboard is needed.
