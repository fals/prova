#!/usr/bin/env bash
# Local mirror of .github/workflows/ci.yml. Run before merging.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --no-fail-fast
( cd spec && for f in *.qnt; do quint typecheck "$f"; quint test "$f" >/dev/null; done )
# Example specifications shipped in the prova-spec skill must stay valid.
( cd skills/prova-spec/examples && for f in *.qnt; do quint typecheck "$f"; quint test "$f" >/dev/null; done
  quint run ipam.qnt --invariant safety --max-steps 20 --max-samples 5000 | grep -q "No violation" )
for s in run experiment; do
  ( cd spec && quint run "$s.qnt" --invariant safety --max-steps 25 --max-samples 20000 | grep -q "No violation" )
done
tmp=$(mktemp -d)
scripts/spec-traces.sh "$tmp/run" 60 "" run
scripts/spec-traces.sh "$tmp/exp" 60 "" experiment
PROVA_TRACE_DIR="$tmp/run" cargo test -p prova-core --test spec_replay
PROVA_TRACE_DIR_EXP="$tmp/exp" cargo test -p prova-core --test spec_replay_experiment
echo "ci: all green"
