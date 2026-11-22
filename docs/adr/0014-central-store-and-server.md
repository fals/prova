# 0014: Central store, partitioned by repository

Status: Accepted (owner, 2026-11-22). Sections 1, 2, 4 and 5 implemented. Section 3, a server, is **deferred** by the owner: toe stays local, also in CI and Kubernetes. Supersedes the storage location in [0008](0008-storage-sqlite.md).

## Context

Today every workspace keeps its own store at `<root>/.toe/toe.db` and there is no daemon ([0001](0001-architecture-overview.md)). That breaks down in three situations:

1. **Many repositories on one machine.** One engineer, or many agents, work across tens of repositories and their git worktrees. History is scattered per folder, split across worktrees of the same repository, lost when a folder is moved or recloned, and there is no view across repositories.
2. **Clusters.** When tests and experiments run in pods (a kind cluster during development, a real cluster in CI or for load tests), each pod's filesystem is ephemeral. A store inside the workspace disappears with the pod, and results from parallel pods never meet. Everything toe records (runs, flake statistics, experiment trials) is lost exactly where it matters most.
3. **Experiments across machines.** Trials must be compared on a known machine fingerprint ([0007](0007-experiments.md)); that only works if trials from every node land in one place that can see all fingerprints.

The owner's direction: keep the database centralized per machine and split per repository; keep toe local and delay cluster-wide capture (section 3).

## Decision

### 1. One logical store, partitioned by repository

toe has a single store per installation (a machine user), not per folder. Inside it, data is partitioned by **repository**:

- **Repository id**: hash of the repository's root commit (`git rev-list --max-parents=0 HEAD`, the oldest if several). Identical for every clone, worktree and pod of the same repository. Outside git: hash of the `origin`-less root path, marked as local-only.
- Recorded with it: `origin` URL (credentials stripped), display name, and every workspace path it was seen at.
- Every run, test, attempt, artifact, experiment and trial belongs to exactly one repository: the repository's own file holds it, so ids stay unchanged ([0006](0006-identity-history-flakiness.md)) and cross-repository views qualify them with the repository id.
- Each run also records **where it ran**: host, container or pod name, node, workspace path and an **actor** (user, CI job or agent session id). Runs from different worktrees, machines or pods of the same repository share history and stay distinguishable.

### 2. Local central store (default, no service)

On a machine, the store lives in one directory, `TOE_HOME` (default `$XDG_DATA_HOME/toe`, i.e. `~/.local/share/toe`):

```
$TOE_HOME/
  catalog.db               repositories, workspace paths, actors (small, rarely written)
  repos/<repo-id>/toe.db   runs, events, tests, attempts, experiments of one repository
  repos/<repo-id>/runs/    captured output, injected reports
  repos/<repo-id>/artifacts/
```

- **One SQLite file per repository** is the physical split. Agents working in different repositories never share a write lock; deleting a repository's history is deleting one directory; a repository's data can be exported or moved on its own.
- **Cross-repository views** (`toe status --all`, `toe flaky --all`, `toe history --all <test>`) open the catalog and ATTACH the repository files read-only.
- The workspace keeps nothing. `.toe/` in the workspace remains only as a fallback when `TOE_HOME` is not writable, and toe says so in a run note.
- Still no daemon: the CLI and per-run supervisors write directly, as today.

### 3. No server for now: local storage everywhere (deferred)

A `toe serve` design (one service owning the store, clients replicating runs to
it) was prototyped in [PR #49](https://github.com/fals/toe/pull/49) and closed:
the owner chose to keep toe local and delay that complexity. Tests run
sandboxed with local storage in every environment:

| Where | How history is kept |
|---|---|
| Developer machine, agent sandbox | `~/.local/share/toe` (default home) |
| GitHub Actions, GitLab CI | `TOE_HOME` inside the job; persisted between jobs with the CI cache (`actions/cache`, GitLab `cache:`) or job artifacts |
| Container, devcontainer | `TOE_HOME` on a mounted volume |
| Kubernetes | `TOE_HOME` on a volume mounted by one writer at a time (ReadWriteOnce), or one home per job |

Limits accepted for now:

- **SQLite on shared network storage** (NFS, ReadWriteMany volumes) is unsafe
  with several concurrent writers: WAL needs one host, and network file locks
  are unreliable. Run one writer per volume, or give each job its own home.
- **Parallel CI jobs** that restore and save the same cache overwrite each
  other's history (last save wins); nothing is merged.
- **No live view from outside**: an agent waits on runs only where they run.

If merging homes or live remote views become necessary, the next step is a
file-level `toe export` / `toe import` of a repository's store (no service),
before reconsidering a server.

### 4. Configuration precedence

`TOE_HOME` (or its default) → local central store. Otherwise (not writable) → workspace `.toe/` with a warning. Every run summary states which store it used.

### 5. Migration

On first use by a version implementing this ADR, an existing workspace `.toe/toe.db` is imported into the central store under the workspace's repository id, then left with a `MOVED` note. Run ids are random and test ids content hashes, so imports from several worktrees never collide.

## Consequences

- One history per repository across folders and worktrees on a machine; flaky detection and experiments see all of it.
- No service to run anywhere. CI and Kubernetes keep history by persisting `TOE_HOME` (cache, artifact or volume), within the limits in section 3.
- Results from different machines or pods do not meet unless their homes are moved together; cross-machine experiments are compared per machine (fingerprints, [0007](0007-experiments.md)).
- Concurrency: writes are per repository file (WAL, short transactions); one writer per volume.

## Alternatives

- **Store inside the repository's git directory, shared by worktrees** (an earlier draft): history would vanish with every container and could not be cached as one directory. Rejected.
- **One SQLite file for all repositories**: simplest queries, but one write lock for every agent on the machine and no clean per-repository deletion or export. Rejected in favour of a file per repository under one home.
- **`toe serve`, a central service** (prototyped in PR #49): live results from pods and CI in one place, at the cost of a service, tokens and a network API. Deferred by the owner.
- **Postgres**: only relevant with a service; deferred with it.
