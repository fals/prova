# 0014: Central store, partitioned by repository, with an optional server

Status: Accepted (owner, 2026-11-22). Sections 1, 2, 4 and 5 implemented; section 3 (`toe serve`) next. Supersedes the storage location in [0008](0008-storage-sqlite.md); amends [0001](0001-architecture-overview.md) (a long-lived service becomes an optional component) and [0010](0010-deployment.md) (the image also runs that service).

## Context

Today every workspace keeps its own store at `<root>/.toe/toe.db` and there is no daemon ([0001](0001-architecture-overview.md)). That breaks down in three situations:

1. **Many repositories on one machine.** One engineer, or many agents, work across tens of repositories and their git worktrees. History is scattered per folder, split across worktrees of the same repository, lost when a folder is moved or recloned, and there is no view across repositories.
2. **Clusters.** When tests and experiments run in pods (a kind cluster during development, a real cluster in CI or for load tests), each pod's filesystem is ephemeral. A store inside the workspace disappears with the pod, and results from parallel pods never meet. Everything toe records (runs, flake statistics, experiment trials) is lost exactly where it matters most.
3. **Experiments across machines.** Trials must be compared on a known machine fingerprint ([0007](0007-experiments.md)); that only works if trials from every node land in one place that can see all fingerprints.

The owner's direction: keep the database centralized, split per repository where needed, and make cluster capture a first-class case.

## Decision

### 1. One logical store, partitioned by repository

toe has a single store per installation (a machine user, or a server), not per folder. Inside it, data is partitioned by **repository**:

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

### 3. Server mode for clusters: `toe serve`

The same store can be owned by a long-running service, shipped in the existing container image:

- `toe serve` exposes the store over HTTPS (JSON API, streaming for events). Behind it: the same per-repository SQLite layout on a persistent volume. A different backend (Postgres) is a later decision, only if one server outgrows SQLite; the API does not change.
- Clients opt in with `TOE_SERVER=https://toe.toe-system.svc:7777` (or `server` in `toe.toml`) and a bearer token (`TOE_TOKEN`).
- **Execution stays local.** The supervisor still runs next to the tests, in the pod or on the machine, because only there can it watch the process tree, kill process groups and read result files ([0004](0004-liveness-and-fail-fast.md)). It streams run records, events and artifacts to the server instead of writing a local file.
- `toe wait`, `toe status`, `toe history`, `toe flaky`, `toe exp ...` query the server, so an agent outside the cluster can wait on a run inside it.
- **No data loss on network trouble.** The supervisor spools events to a local journal and replays them when the server is reachable; events are idempotent on `(run, seq)`. A run whose spool never reaches the server is reported as such, not silently dropped.
- **Experiments in a cluster**: trials record the node and its fingerprint; a verdict over trials from different fingerprints is invalid ([0007](0007-experiments.md)). `toe exp` accepts a node constraint so a controller can pin trials to one node.
- **Kubernetes packaging**: a Deployment (one replica, it owns the volume), a PersistentVolumeClaim, a Service, and a Secret with tokens. Test pods get the binary by `COPY --from` ([0010](0010-deployment.md)) and `TOE_SERVER`/`TOE_TOKEN` from the environment. A Helm chart or plain manifests live in `deploy/`.
- **Security**: TLS, per-client tokens, a token scoped to a set of repository ids for CI; commands and output are stored as given ([0008](0008-storage-sqlite.md)), so the server's volume is treated like a log store with retention (`toe gc`).

### 4. Configuration precedence

`TOE_SERVER` set → server. Otherwise `TOE_HOME` (or its default) → local central store. Otherwise (not writable) → workspace `.toe/` with a warning. Every run summary states which store it used.

### 5. Migration

On first use by a version implementing this ADR, an existing workspace `.toe/toe.db` is imported into the central store under the workspace's repository id, then left with a `MOVED` note. Run ids are random and test ids content hashes, so imports from several worktrees never collide. `toe push --server` uploads a local central store into a server the same way.

## Consequences

- One history per repository across folders, worktrees, machines and pods; flaky detection and experiments see all of it.
- Clusters become a supported environment instead of a place where results vanish.
- toe gains a service to run, but only where wanted: local use stays daemon-free.
- The store layer needs a boundary with two implementations (local files, remote server). The engine already talks to the store through one type; that type becomes an interface with a local and an HTTP implementation, and the medium tests run against both.
- Concurrency: local writes are per repository file (WAL, short transactions); on the server one process serializes writes per repository.
- New attack surface (network API, tokens); it needs its own security review before release.

## Alternatives

- **Store inside the repository's git directory, shared by worktrees** (the previous draft of this ADR): fixes worktrees but not clusters, ephemeral containers, or cross-machine experiments. Rejected.
- **One SQLite file for all repositories**: simplest queries, but one write lock for every agent on the machine and no clean per-repository deletion or export. Rejected in favour of a file per repository under one home.
- **Server only**: forces a service even for one developer on a laptop. Rejected; the server is opt-in.
- **Postgres from the start**: operationally heavier for a single-replica service whose load is test events; kept as a later backend behind the same API.
