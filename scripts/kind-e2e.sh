#!/usr/bin/env bash
# End to end in a kind cluster (ADR 0014): toe serve in toe-system, a test
# Job running a fixture in another namespace, and a client outside the
# cluster that waits on the pod's run through the server.
#
# Uses its own cluster (TOE_E2E_CLUSTER, default toe-e2e) and deletes it at
# the end unless TOE_E2E_KEEP=1. Needs docker, kind, kubectl, jq, curl.
set -euo pipefail
cd "$(dirname "$0")/.."
cluster=${TOE_E2E_CLUSTER:-toe-e2e}
ctx="kind-$cluster"
k() { kubectl --context "$ctx" "$@"; }
fail() { echo "e2e: FAIL: $*" >&2; exit 1; }

cargo build -q -p toe-cli
client=$PWD/target/debug/toe

echo "e2e: building images"
docker build -q -t toe:dev . >/dev/null
docker build -q -t toe-fixture-python -f fixtures/docker/python.Dockerfile fixtures/docker >/dev/null
docker build -q -t toe-e2e-tests -f - . >/dev/null <<'DOCKER'
FROM toe-fixture-python
COPY fixtures/python-make /w
DOCKER

created=0
if ! kind get clusters 2>/dev/null | grep -qx "$cluster"; then
  echo "e2e: creating kind cluster $cluster"
  kind create cluster --name "$cluster" --wait 180s >/dev/null
  created=1
fi
pf=""
cleanup() {
  [ -n "$pf" ] && kill "$pf" 2>/dev/null || true
  if [ "$created" = 1 ] && [ "${TOE_E2E_KEEP:-0}" != 1 ]; then
    kind delete cluster --name "$cluster" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT
kind load docker-image --name "$cluster" toe:dev toe-e2e-tests >/dev/null

echo "e2e: deploying toe serve"
token=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')
k apply -f deploy/k8s/toe-serve.yaml >/dev/null
for ns in toe-system toe-e2e; do
  k create namespace "$ns" --dry-run=client -o yaml | k apply -f - >/dev/null
  k -n "$ns" create secret generic toe-tokens --from-literal=tokens="$token" \
    --dry-run=client -o yaml | k apply -f - >/dev/null
done
k -n toe-system set image deploy/toe toe=toe:dev >/dev/null
k -n toe-system rollout restart deploy/toe >/dev/null
k -n toe-system rollout status deploy/toe --timeout=180s >/dev/null

echo "e2e: running the python fixture as a Job"
job="tests-$(date +%s)"
sed -e "s/name: tests$/name: $job/" -e "s#your-project-with-toe:latest#toe-e2e-tests#" \
    -e 's#\["toe", "run", "--", "make", "test"\]#["toe", "run", "--", "make", "test"]#' \
    deploy/k8s/example-test-job.yaml | k -n toe-e2e apply -f - >/dev/null
for _ in $(seq 1 120); do
  st=$(k -n toe-e2e get job "$job" -o jsonpath='{.status.succeeded}{"/"}{.status.failed}')
  [ "$st" != "/" ] && break
  sleep 2
done
pod=$(k -n toe-e2e get pods -l job-name="$job" -o jsonpath='{.items[0].metadata.name}')
node=$(k -n toe-e2e get pods -l job-name="$job" -o jsonpath='{.items[0].spec.nodeName}')
[ -n "$pod" ] || fail "job pod not found"

echo "e2e: reading results from outside the cluster"
port=$((20000 + RANDOM % 20000))
k -n toe-system port-forward svc/toe "$port:7777" >/dev/null 2>&1 &
pf=$!
for _ in $(seq 1 50); do curl -fsS "http://127.0.0.1:$port/v1/health" >/dev/null 2>&1 && break; sleep 0.2; done
export TOE_SERVER="http://127.0.0.1:$port" TOE_TOKEN="$token" TOE_HOME="$(mktemp -d)"
repo=$(curl -fsS -H "Authorization: Bearer $token" "$TOE_SERVER/v1/repos" | jq -r '.[] | select(.workspace=="/w") | .id')
[ -n "$repo" ] || fail "repository of the pod not on the server"
export TOE_REPO="$repo"
cd "$(mktemp -d)"
runs=$("$client" status --json)
run=$(echo "$runs" | jq -r '.[0].id')
[ "$(echo "$runs" | jq -r '.[0].origin.pod')" = "$pod" ] || fail "origin pod: $(echo "$runs" | jq -c '.[0].origin')"
[ "$(echo "$runs" | jq -r '.[0].origin.node')" = "$node" ] || fail "origin node"
set +e
summary=$("$client" wait "$run" --until done --max-wait 30s --json); code=$?
set -e
[ "$code" = 1 ] || fail "wait exit $code, want 1 (test failure): $summary"
echo "$summary" | jq -e '.outcome == "failed"' >/dev/null || fail "outcome: $summary"
echo "$summary" | jq -e '[.failures[].test] | any(contains("test_div"))' >/dev/null || fail "test_div not reported: $summary"
"$client" output "$run" --tail 5 | grep -q "test_div" || fail "output not uploaded"
"$client" history test_div --json | jq -e '.failures >= 1' >/dev/null || fail "history"
echo "e2e: run $run from pod $pod on $node: $(echo "$summary" | jq -r '.counts | "\(.passed) passed, \(.failed) failed"'), bug found"
echo "e2e: all green"
