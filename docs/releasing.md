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
3. The version in `Cargo.toml` (`[workspace.package]`) is the one to tag.

## Tag

```sh
git switch main && git pull
git tag -a v0.1.0 -m "prova 0.1.0"
git push origin v0.1.0
```

The release workflow then, in order:

1. builds each target (Linux musl x86_64 and aarch64, macOS arm64 and
   x86_64), runs `scripts/smoke.sh` on each binary on its own platform
   (Intel macOS through Rosetta), checks the Linux binaries are static and
   runs `install.sh` on each platform;
2. only if all passed: checks the tag matches the version, creates the
   GitHub release with the tarballs, checksums and `install.sh`, and pushes
   `ghcr.io/fals/prova:<tag>` and `:latest` for amd64 and arm64.

## After the first release (once)

GitHub creates new container packages as private. Make it public in the
package settings (`https://github.com/users/fals/packages/container/prova/settings`,
Danger Zone, Change visibility), or `docker pull ghcr.io/fals/prova`
fails for everyone else.

## Check what was published

From a machine that never built prova:

```sh
curl -fsSL https://raw.githubusercontent.com/fals/prova/main/install.sh | sh
prova --version
docker run --rm ghcr.io/fals/prova:latest --version
```
