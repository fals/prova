#!/bin/sh
# Oracle for spec-gated experiments: succeeds only if the Lean
# specification's theorems hold (lake build) and CountPrimes gives the
# specified answer for every n below 3000.
set -e
cd "$(dirname "$0")"
lake build -q
lake exe reference 3000 > .lake/reference.txt
cd ..
go run ./cmd/conform < spec/.lake/reference.txt
