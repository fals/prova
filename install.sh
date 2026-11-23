#!/bin/sh
# Installs the prova binary from the GitHub release.
#   curl -fsSL https://raw.githubusercontent.com/fals/prova/main/install.sh | sh
# Environment: PROVA_VERSION (a tag such as v0.1.0; default: the latest
# release), PROVA_INSTALL_DIR (default: ~/.local/bin).
set -eu

repo=fals/prova
version=${PROVA_VERSION:-latest}
dir=${PROVA_INSTALL_DIR:-$HOME/.local/bin}

case "$(uname -s)" in
  Linux) os=unknown-linux-musl ;;
  Darwin) os=apple-darwin ;;
  *) echo "prova: unsupported OS $(uname -s); build from source: cargo install --git https://github.com/$repo prova-cli" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) echo "prova: unsupported CPU $(uname -m)" >&2; exit 1 ;;
esac
asset="prova-$arch-$os.tar.gz"
if [ -n "${PROVA_BASE_URL:-}" ]; then
  base=$PROVA_BASE_URL # a mirror, or CI testing this script
elif [ "$version" = latest ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/$version"
fi

fetch() {
  if command -v curl >/dev/null 2>&1; then curl -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then wget -q "$1" -O "$2"
  else echo "prova: needs curl or wget" >&2; exit 1
  fi
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "prova: downloading $asset ($version)"
fetch "$base/$asset" "$tmp/$asset"
fetch "$base/$asset.sha256" "$tmp/$asset.sha256"
expected=$(cut -d' ' -f1 "$tmp/$asset.sha256")
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$asset" | cut -d' ' -f1)
else
  actual=$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)
fi
[ "$expected" = "$actual" ] || { echo "prova: checksum mismatch for $asset" >&2; exit 1; }

tar xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$dir"
install -m 0755 "$tmp/prova-$arch-$os/prova" "$dir/prova"
echo "prova: installed $("$dir/prova" --version) to $dir/prova"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "prova: add $dir to your PATH" ;;
esac
echo "next: cd your-project && prova run -- <your test command>"
