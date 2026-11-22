#!/bin/sh
# Runs the fixture's three prepared changes as toe experiments against the
# committed baseline and prints one line: faster=<v> same=<v> slower=<v>.
#
# usage: exp-check.sh <metric> <primes module path> [--setup <shell cmd>] -- <benchmark command...>
# Run from a copy of a fixture; needs git and toe on PATH.
set -eu
metric=$1; module=$2; shift 2
setup=""
if [ "${1:-}" = "--setup" ]; then setup=$2; shift 2; fi
[ "${1:-}" = "--" ] && shift
out=$(mktemp -d)
g() { git -c user.email=fixture@toe -c user.name=fixture "$@"; }
[ -d .git ] || { g init -q && g add -A && g commit -qm baseline; }
set -- toe exp new primes --hypothesis "the prepared change alters count-primes speed" \
  --metric "$metric" --better lower --min-effect 5% --trials 5 --min-trials 5 --max-trials 5 \
  --warmup 1 ${setup:+--setup "$setup"} -- "$@"
"$@" >/dev/null
line=""
for v in faster same slower; do
  cp "experiments/$v/$module.txt" "$module"
  toe exp candidate primes --label "$v" >/dev/null
  toe exp run primes --candidate "$v" --against baseline --json > "$out/$v.json" || true
  g checkout -q -- "$module"
  verdict=$(grep -o '"verdict":"[a-z]*"' "$out/$v.json" | head -1 | cut -d'"' -f4)
  status=$(grep -o '"status":"[a-z]*"' "$out/$v.json" | head -1 | cut -d'"' -f4)
  line="$line $v=${verdict:-$status}"
done
echo "${line# }"
