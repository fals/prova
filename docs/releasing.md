# Releasing

A release is a `v*` tag on `main`. Nothing is published before every
shipped binary has passed on its own platform.

## Before tagging

1. `main` is green in CI: the suite on Linux x86_64, Linux arm64 and
   macOS, the spec job, and the package job (the musl binary installed by
   `install.sh`, smoke-tested, and the release image).
2. The release workflow's builds passed on the last PR that touched the
   release path (`release.yml`, `Dockerfile.release`, `install.sh`,
   `Cargo.lock`).
3. **Release candidate accepted.** CI green is not enough: it shows only that prova's own tests pass. Build the candidate from `main` and run, on that binary:
   - the regression probes of every earlier usability round (their blocker repros, kept as large tests or corpus cases);
   - a short acceptance round: the adversarial probe agent, plus a fixture agent for each area the release changed, using the real tools (Docker fixtures for containers).
   Any blocker or false pass means no tag: fix it, then start this step again. Record the candidate's run ids in the release PR.
4. The version in `Cargo.toml` (`[workspace.package]`) is the one to tag:
   bump it (and the `vX.Y.Z` pins in README, getting started and
   `llms.txt`, and, for a new minor or major version, the `version` of the
   internal `prova-*` dependencies in `crates/*/Cargo.toml`) in a PR before
   tagging. The release workflow refuses a tag that differs from the
   `Cargo.toml` version; it does not check the doc pins or the internal
   dependency versions, so check them in the PR.

## Tag

```sh
git switch main && git pull
git tag -a vX.Y.Z -m "prova X.Y.Z"
git push origin vX.Y.Z
```

The release workflow then, in order:

1. builds each target (Linux musl x86_64 and aarch64, macOS arm64 and
   x86_64), runs `scripts/smoke.sh` on each binary on its own platform
   (Intel macOS through Rosetta), checks the Linux binaries are static and
   runs `install.sh` on each platform;
2. only if all passed: checks the tag matches the version, creates the
   GitHub release with the tarballs, checksums and `install.sh`, and pushes
   `ghcr.io/fals/prova:<tag>` and `:latest` for amd64 and arm64.

## Image visibility

The image inherits the public repository's visibility (it carries the
`org.opencontainers.image.source` label): v0.1.0 was pullable anonymously
right after publishing. If a package ever shows as private, change it in
`https://github.com/users/fals/packages/container/prova/settings`.

## Check what was published

From a machine that never built prova:

```sh
curl -fsSL https://raw.githubusercontent.com/fals/prova/main/install.sh | sh
prova --version
docker run --rm ghcr.io/fals/prova:<tag> --version
```
