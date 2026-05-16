# syntax=docker/dockerfile:1.7
#
# GrokRxiv orchestrator — multi-stage Rust build.
# Stage 1: compile a release binary against musl-friendly bookworm.
# Stage 2: minimal Debian slim runtime with CA certs + libssl.

FROM rust:1.82-slim AS builder
WORKDIR /app

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      pkg-config \
      libssl-dev \
      ca-certificates \
      git \
 && rm -rf /var/lib/apt/lists/*

# The `rust:1.82-slim` image already pins toolchain 1.82.0 — we deliberately
# do NOT copy `rust-toolchain.toml` so rustup doesn't try to fetch optional
# components (clippy/rustfmt) that aren't needed for a release build and are
# the most common cause of transient registry failures.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# The orchestrator embeds the per-role JSON schemas at compile time via
# `include_str!("../../../schemas/...")`, so the schemas tree must be present
# during the build stage (not just the runtime stage).
COPY schemas ./schemas

ENV RUSTUP_TOOLCHAIN=1.82.0
RUN cargo build --release -p grokrxiv-orchestrator

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates \
      libssl3 \
      tini \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home --shell /usr/sbin/nologin grokrxiv

COPY --from=builder /app/target/release/grokrxiv-orchestrator /usr/local/bin/orchestrator
COPY agents  /etc/grokrxiv/agents
COPY schemas /etc/grokrxiv/schemas
COPY prompts /etc/grokrxiv/prompts

ENV ORCHESTRATOR_BIND=0.0.0.0:8080 \
    GROKRXIV_AGENTS_DIR=/etc/grokrxiv/agents \
    GROKRXIV_SCHEMAS_DIR=/etc/grokrxiv/schemas \
    GROKRXIV_PROMPTS_DIR=/etc/grokrxiv/prompts \
    RUST_LOG=info

USER grokrxiv
EXPOSE 8080
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/orchestrator"]
