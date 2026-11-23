#!/bin/sh
# End-to-end smoke test of one prova binary on the machine it runs on:
# the release workflow runs it on every shipped binary before publishing.
#   scripts/smoke.sh path/to/prova
set -eu
prova=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export PROVA_HOME="$work/home"
cd "$work"
mkdir p && cd p
git init -q
git config user.email smoke@prova.invalid
git config user.name smoke

fail() { echo "smoke: FAIL: $*" >&2; exit 1; }
expect() { # expect <exit code> <text> -- prova args...
  code=$1 text=$2; shift 3
  set +e; out=$("$prova" "$@" 2>&1); got=$?; set -e
  [ "$got" = "$code" ] || fail "prova $* exited $got, expected $code:
$out"
  case "$out" in *"$text"*) ;; *) fail "prova $* did not print '$text':
$out" ;; esac
  echo "smoke: ok: prova $* -> $got"
}

# A test command in libtest's format, doing real work.
cat > t.sh <<'T'
i=0; while [ $i -lt 50000 ]; do i=$((i+1)); done
echo "running 2 tests"
echo "test calc::adds ... ok"
if [ -f broken ]; then
  echo "test calc::divides ... FAILED"; echo "test result: FAILED. 1 passed; 1 failed"; exit 101
fi
echo "test calc::divides ... ok"; echo "test result: ok. 2 passed; 0 failed"
T
git add t.sh && git commit -qm init

"$prova" --version
expect 0 "PASSED" -- run -- sh t.sh
touch broken
expect 1 "calc::divides" -- run --until done -- sh t.sh
rm broken
expect 0 "PASSED" -- run --until done -- sh t.sh
expect 0 "calc::divides" -- history calc::divides

# Silent but busy for longer than the stall limit: CPU is activity, so
# this must not stall (process-group sampling works on this OS).
cat > busy.sh <<'T'
end=$(( $(date +%s) + 6 ))
while [ "$(date +%s)" -lt "$end" ]; do :; done
echo "test busy::works ... ok"
T
expect 0 "PASSED" -- run --until done --stall-after 2s -- sh busy.sh

# Silent and idle past the limit: a stall, named and ended.
expect 3 "stall" -- run --until done --stall-after 2s -- sh -c 'sleep 30'
expect 0 "" -- cancel last
echo "smoke: all good"
