# syntax=docker/dockerfile:1

FROM oven/bun:1.4.5@sha256:3aa040e5445efa09d75bb428a64ba50db13e2b3ac9b6b0c7f145615f62b2ee18 AS bun

FROM rust:1-slim-bookworm@sha256:7c7d4eb4d05b7781187a327809466cfddf29753f6defdc3d19a0e291951b7a37 AS builder

RUN apt-get update \
    && apt-get install --yes --no-install-recommends libssl-dev pkg-config \
    && rm --recursive --force /var/lib/apt/lists/*

COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun

WORKDIR /app

COPY . .

RUN bun install --no-save --cwd ui --registry=https://registry.npmjs.org/ \
    && cargo build --locked --release --bin keeppeek \
    && mkdir --parents /container-config/keeppeek

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

ENV XDG_CONFIG_HOME=/config

COPY --from=builder /app/target/release/keeppeek /keeppeek
COPY --from=builder --chmod=0644 /app/skills/keeppeek-mcp/SKILL.md /usr/local/share/keeppeek/skills/keeppeek-mcp/SKILL.md
COPY --from=builder --chown=65532:65532 /container-config/keeppeek /config/keeppeek

VOLUME ["/config"]

EXPOSE 8081

ENTRYPOINT ["/keeppeek"]
