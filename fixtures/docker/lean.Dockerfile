# Go plus Lean 4 (core only, no mathlib): the go-make fixture's Lean
# specification is checked and run as the oracle of a spec-gated experiment.
FROM golang:1.24-bookworm
ENV ELAN_HOME=/opt/elan PATH=/opt/elan/bin:$PATH GOTOOLCHAIN=local
RUN curl -sSfL https://raw.githubusercontent.com/leanprover/elan/master/elan-init.sh \
      | sh -s -- -y --no-modify-path --default-toolchain leanprover/lean4:v4.19.0 \
 && lean --version \
 && chmod -R a+rX /opt/elan
COPY --from=toe:dev /toe /usr/local/bin/toe
