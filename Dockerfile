# Minimal image holding only the static prova binary (ADR 0010).
# Use it to copy prova into your own images:
#   COPY --from=ghcr.io/fals/prova:<version> /prova /usr/local/bin/prova
FROM rust:1.95-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p prova-cli \
 && strip target/release/prova

FROM scratch
COPY --from=build /src/target/release/prova /prova
ENTRYPOINT ["/prova"]
