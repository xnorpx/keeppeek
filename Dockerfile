# syntax=docker/dockerfile:1

FROM oven/bun:1.3.14@sha256:e10577f0db68676a7024391c6e5cb4b879ebd17188ab750cf10024a6d700e5c4 AS bun

FROM rust:1-slim-bookworm@sha256:2775a09d208ff0d7c1f50490c45b62db929e87ba1dcbc3f2132ac71a704bcdd3 AS builder

RUN apt-get update \
    && apt-get install --yes --no-install-recommends libssl-dev pkg-config \
    && rm --recursive --force /var/lib/apt/lists/*

COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun

WORKDIR /app

COPY . .

RUN bun install --no-save --cwd ui --registry=https://registry.npmjs.org/ \
    && cargo build --release --bin keeppeek

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:adcd20c7b4c988b73cbfbddb26d2eee574571e6d7c9ffea29b3821e0690efb77

ENV XDG_CONFIG_HOME=/config

COPY --from=builder /app/target/release/keeppeek /keeppeek

VOLUME ["/config"]

EXPOSE 8081

ENTRYPOINT ["/keeppeek"]
