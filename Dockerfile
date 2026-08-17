# syntax=docker/dockerfile:1

FROM oven/bun:1.3.14 AS bun

FROM rust:1-slim-bookworm AS builder

RUN apt-get update \
    && apt-get install --yes --no-install-recommends libssl-dev pkg-config \
    && rm --recursive --force /var/lib/apt/lists/*

COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun

WORKDIR /app

COPY . .

RUN bun install --cwd ui --registry=https://registry.npmjs.org/ \
    && cargo build --release --bin keeppeek

FROM gcr.io/distroless/cc-debian12:nonroot

ENV XDG_CONFIG_HOME=/config

COPY --from=builder /app/target/release/keeppeek /keeppeek

VOLUME ["/config"]

EXPOSE 8081

ENTRYPOINT ["/keeppeek"]
