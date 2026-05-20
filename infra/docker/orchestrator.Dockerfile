# syntax=docker/dockerfile:1.7
#
# GrokRxiv orchestrator — multi-stage Rust build.
# Stage 1: compile a release binary against musl-friendly bookworm.
# Stage 2: Node/Debian slim runtime with CA certs, libssl, tini, optional
# Pandoc, and optional provider CLIs.

ARG PANDOC_VERSION=3.9.0.2
ARG PANDOC_SHA256_AMD64=a69abfababda8a56969a254b09f9553a7be89ddec00d4e0fe9fd585d71a67508
ARG PANDOC_SHA256_ARM64=b6d21e8f9c3b15744f5a7ab40248019157ed7793875dbe0383d4c82ff572b528
ARG INSTALL_PANDOC=1
ARG INSTALL_AGENT_CLIS=1

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
# The HTML/PR quality prompts are also embedded with `include_str!`.
COPY prompts ./prompts

ENV RUSTUP_TOOLCHAIN=1.82.0
RUN cargo build --release -p grokrxiv-orchestrator

FROM node:20-bookworm-slim AS runtime

ARG TARGETARCH
ARG PANDOC_VERSION
ARG PANDOC_SHA256_AMD64
ARG PANDOC_SHA256_ARM64
ARG INSTALL_PANDOC
ARG INSTALL_AGENT_CLIS

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
      ca-certificates \
      git \
      gosu \
      libssl3 \
      ripgrep \
      tini; \
    if [ "$INSTALL_PANDOC" = "1" ]; then \
      apt-get install -y --no-install-recommends curl tar; \
      case "$TARGETARCH" in \
        amd64) pandoc_arch="amd64"; pandoc_sha="$PANDOC_SHA256_AMD64" ;; \
        arm64) pandoc_arch="arm64"; pandoc_sha="$PANDOC_SHA256_ARM64" ;; \
        *) echo "unsupported Pandoc TARGETARCH=$TARGETARCH" >&2; exit 1 ;; \
      esac; \
      pandoc_url="https://github.com/jgm/pandoc/releases/download/${PANDOC_VERSION}/pandoc-${PANDOC_VERSION}-linux-${pandoc_arch}.tar.gz"; \
      curl -fsSL "$pandoc_url" -o /tmp/pandoc.tar.gz; \
      printf '%s  %s\n' "$pandoc_sha" /tmp/pandoc.tar.gz | sha256sum -c -; \
      mkdir -p /tmp/pandoc; \
      tar -xzf /tmp/pandoc.tar.gz -C /tmp/pandoc --strip-components=1; \
      install -m 0755 /tmp/pandoc/bin/pandoc /usr/local/bin/pandoc; \
      rm -rf /tmp/pandoc /tmp/pandoc.tar.gz; \
      apt-get purge -y --auto-remove curl; \
    fi; \
    if [ "$INSTALL_AGENT_CLIS" = "1" ]; then \
      npm install -g @anthropic-ai/claude-code @openai/codex @google/gemini-cli; \
      node_arch="$(node -p 'process.arch')"; \
      mkdir -p /usr/local/lib/node_modules/@google/gemini-cli/bundle/vendor/ripgrep; \
      ln -sf /usr/bin/rg "/usr/local/lib/node_modules/@google/gemini-cli/bundle/vendor/ripgrep/rg-linux-${node_arch}"; \
      npm cache clean --force; \
    fi; \
    rm -rf /var/lib/apt/lists/*; \
    useradd --system --create-home --shell /usr/sbin/nologin grokrxiv

COPY --from=builder /app/target/release/grokrxiv-orchestrator /usr/local/bin/orchestrator
COPY agents  /etc/grokrxiv/agents
COPY schemas /etc/grokrxiv/schemas
COPY prompts /etc/grokrxiv/prompts
COPY infra/docker/orchestrator-entrypoint.sh /usr/local/bin/grokrxiv-orchestrator-entrypoint
RUN chmod 0755 /usr/local/bin/grokrxiv-orchestrator-entrypoint

ENV ORCHESTRATOR_BIND=0.0.0.0:8080 \
    GROKRXIV_AGENTS_DIR=/etc/grokrxiv/agents \
    GROKRXIV_SCHEMAS_DIR=/etc/grokrxiv/schemas \
    GROKRXIV_PROMPTS_DIR=/etc/grokrxiv/prompts \
    GROKRXIV_CLI_AUTH_SOURCE=/run/secrets/grokrxiv-cli-auth \
    GROKRXIV_CLI_AUTH_HOME=/home/grokrxiv \
    HOME=/home/grokrxiv \
    RUST_LOG=info

EXPOSE 8080
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/grokrxiv-orchestrator-entrypoint"]
CMD ["/usr/local/bin/orchestrator", "serve"]
