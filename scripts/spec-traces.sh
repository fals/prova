#!/usr/bin/env bash
# Generate ITF traces from spec/run.qnt for the conformance test.
# Usage: scripts/spec-traces.sh <out-dir> [n-traces] [seed]
set -euo pipefail
out=${1:?out dir}; n=${2:-40}; seed=${3:-}
mkdir -p "$out"
cd "$(dirname "$0")/../spec"
quint run run.qnt --mbt --max-steps 20 --n-traces "$n" --max-samples "$n" \
  ${seed:+--seed "$seed"} --out-itf="$out/run_{seq}.itf.json" >/dev/null
