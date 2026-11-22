#!/usr/bin/env bash
# Generate ITF traces from a spec for the conformance tests.
# Usage: scripts/spec-traces.sh <out-dir> [n-traces] [seed] [spec: run|experiment]
set -euo pipefail
out=${1:?out dir}; n=${2:-40}; seed=${3:-}; spec=${4:-run}
mkdir -p "$out"
cd "$(dirname "$0")/../spec"
quint run "$spec.qnt" --mbt --max-steps 20 --n-traces "$n" --max-samples "$n" \
  ${seed:+--seed "$seed"} --out-itf="$out/${spec}_{seq}.itf.json" >/dev/null
