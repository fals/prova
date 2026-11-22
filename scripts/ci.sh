#!/usr/bin/env bash
# Local mirror of .github/workflows/ci.yml. Run before merging.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --no-fail-fast
( cd spec && for f in *.qnt; do quint typecheck "$f"; quint test "$f" >/dev/null; done )
for s in run experiment; do
  ( cd spec && quint run "$s.qnt" --invariant safety --max-steps 25 --max-samples 20000 | grep -q "No violation" )
done
tmp=$(mktemp -d)
scripts/spec-traces.sh "$tmp/run" 60 "" run
scripts/spec-traces.sh "$tmp/exp" 60 "" experiment
TOE_TRACE_DIR="$tmp/run" cargo test -q -p toe-core --test spec_replay
TOE_TRACE_DIR_EXP="$tmp/exp" cargo test -q -p toe-core --test spec_replay_experiment
echo "ci: all green"
