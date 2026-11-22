#!/bin/sh
# Builds the JMH uber-jar (profile "bench") and runs the primes benchmark.
set -e
cd "$(dirname "$0")"
mvn -q -B -Pbench package -DskipTests
exec java -jar target/benchmarks.jar -f 1 -wi 1 -w 200ms -i 3 -r 200ms -bm avgt -tu ms "$@"
