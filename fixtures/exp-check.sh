#!/bin/sh
# Runs the fixture's prepared changes as prova experiments against the
# committed baseline and prints one line: faster=<v> same=<v> slower=<v>.
# A candidate rejected by the specification's oracle prints reject(spec).
#
# usage: exp-check.sh <metric> <primes module path> [--setup <cmd>]
#          [--spec <file> --oracle <cmd>] [--variants "<v1> <v2>..."] -- <benchmark command...>
# Run from a copy of a fixture; needs git and prova on PATH.
set -eu
metric=$1; module=$2; shift 2
setup=""; spec=""; oracle=""; variants="faster same slower"
while [ $# -gt 0 ] && [ "$1" != "--" ]; do
  case $1 in
    --setup) setup=$2 ;;
    --spec) spec=$2 ;;
    --oracle) oracle=$2 ;;
    --variants) variants=$2 ;;
    *) echo "exp-check: unknown option $1" >&2; exit 2 ;;
  esac
  shift 2
done
[ "${1:-}" = "--" ] && shift
out=$(mktemp -d)
g() { git -c user.email=fixture@prova -c user.name=fixture "$@"; }
[ -d .git ] || { g init -q && g add -A && g commit -qm baseline; }
set -- prova exp new primes --hypothesis "the prepared change alters count-primes speed" \
  --metric "$metric" --better lower --min-effect 5% --trials 5 --min-trials 5 --max-trials 5 \
  --warmup 1 ${setup:+--setup "$setup"} ${spec:+--spec "$spec"} ${oracle:+--oracle "$oracle"} -- "$@"
"$@" >/dev/null
line=""
for v in $variants; do
  cp "experiments/$v/$module.txt" "$module"
  prova exp candidate primes --label "$v" >/dev/null
  prova exp run primes --candidate "$v" --against baseline --json > "$out/$v.json" || true
  g checkout -q -- "$module"
  verdict=$(grep -o '"verdict":"[a-z]*"' "$out/$v.json" | head -1 | cut -d'"' -f4)
  status=$(grep -o '"status":"[a-z]*"' "$out/$v.json" | head -1 | cut -d'"' -f4)
  grep -q 'violates the specification' "$out/$v.json" && verdict="$verdict(spec)"
  line="$line $v=${verdict:-$status}"
done
echo "${line# }"
