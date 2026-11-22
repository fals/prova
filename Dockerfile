# Minimal image holding only the static toe binary (ADR 0010).
# Use it to copy toe into your own images:
#   COPY --from=ghcr.io/fals/toe:<version> /toe /usr/local/bin/toe
FROM rust:1.95-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p toe-cli \
 && strip target/release/toe

FROM scratch
COPY --from=build /src/target/release/toe /toe
ENTRYPOINT ["/toe"]
