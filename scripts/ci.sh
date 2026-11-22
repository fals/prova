#!/usr/bin/env bash
# Local mirror of .github/workflows/ci.yml. Run before merging.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --no-fail-fast
( cd spec && for f in *.qnt; do quint typecheck "$f"; quint test "$f" >/dev/null; done )
( cd spec && quint run run.qnt --invariant safety --max-steps 25 --max-samples 20000 | grep -q "No violation" )
tmp=$(mktemp -d)
scripts/spec-traces.sh "$tmp" 60
TOE_TRACE_DIR="$tmp" cargo test -q -p toe-core --test spec_replay
echo "ci: all green"
