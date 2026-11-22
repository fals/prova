# 0010: Deployment: static binary, container image as packaging

Status: Proposed. **Deviates from the README**, which says "always container".

## Context

toe must execute the system under test's own test command (`cargo test`, `go test`, `pytest`), with that project's toolchain, in that project's workspace, and be reachable from the harness's shell. Those live wherever the agent works: usually the host, sometimes a devcontainer or CI container.

A toe that exists only inside its own container cannot see host toolchains. The workarounds are mounting the Docker socket (root-equivalent, path mismatches) or forcing all tests into containers (changes how the project tests). Details in [02](../research/02-conduit-skills-deployment.md).

## Decision

1. **The product is one statically linked binary** (`x86_64` and `aarch64`, Linux musl first, macOS next). It runs where the tests run.
2. **A container image is published for every release** (`ghcr.io/fals/toe`), minimal, containing only the binary. Uses:
   - `COPY --from=ghcr.io/fals/toe /toe /usr/local/bin/toe` into devcontainers, CI images and agent sandboxes: the easy, pinned way to deploy;
   - direct `docker run -v $PWD:/work` when the project's test toolchain is itself containerised.
3. **No daemon to deploy** ([0001](0001-architecture-overview.md)). If a long-lived service appears later (dashboard, shared history), it ships as a container and the binary stays the client.
4. Install paths: container copy, release tarball, `cargo install`.

## Consequences

- Zero path mapping, zero socket exposure, works in host, devcontainer and CI the same way.
- Container remains the uniform distribution and pinning mechanism, which serves the README's "easy to deploy" goal.
- Per-platform release builds are required.

## Alternatives

- Container-only with workspace and Docker socket mounted: security and path problems, host toolchains still invisible.
- Engine container plus thin host client (Dagger style): justified only when tests themselves run in containers.

## Open question for review

If "always container" meant "agents always work inside containers", this ADR already fits: toe is copied into that container. If it meant "toe itself must only run as a container", the trade-offs above apply and this needs discussion.
