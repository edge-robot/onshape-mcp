# Fork-specific container build (edge-robot), owned by onshape-architekt.
# Upstream's Dockerfile stays untouched. Stages:
#   base    - pinned Rust toolchain + musl target
#   dev     - devcontainer for rust-mcp-dev (Claude Code, nextest, cargo-deny, ...)
#   builder - reproducible static release binary
#   runtime - minimal production image for Clawake (no shell, no dev tools)
#
#   podman build --target dev     -t localhost/onshape-mcp-dev:latest .
#   podman build --target runtime -t localhost/clawonshape-mcp:<sha>-<label> .

ARG RUST_VERSION=1.99

FROM docker.io/library/rust:${RUST_VERSION}-slim-trixie AS base
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config musl-tools ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# rust-toolchain.toml pins channel "1.99"; install exactly that toolchain name (the base
# image's is "1.99.0"), otherwise rustup auto-installs a second one without musl.
# Named RUST_CHANNEL because the base image's ENV RUST_VERSION would shadow an ARG.
ARG RUST_CHANNEL=1.99
RUN rustup toolchain install "${RUST_CHANNEL}" --profile minimal \
        --component rustfmt,clippy,llvm-tools-preview --target x86_64-unknown-linux-musl \
    && rustup default "${RUST_CHANNEL}" \
    && chmod -R a+rwX "${RUSTUP_HOME}"
WORKDIR /workspaces/onshape-mcp

FROM docker.io/library/node:24-trixie-slim AS node

FROM base AS dev
# Versions mirror mise.toml / mise.lock.
ARG CLAUDE_CODE_VERSION=2.1.300
ARG NEXTEST_VERSION=0.9.148
ARG CARGO_DENY_VERSION=0.20.2
ARG LLVM_COV_VERSION=0.9.1
ARG MDBOOK_VERSION=0.5.4
RUN apt-get update \
    && apt-get install -y --no-install-recommends git curl python3 ripgrep less procps \
    && rm -rf /var/lib/apt/lists/*
# Node is needed by scripts/sync-versions.js and Claude Code.
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx \
    && npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}"
RUN curl -L --proto '=https' --tlsv1.2 -sSf \
        https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash \
    && cargo binstall -y --locked \
        "cargo-nextest@${NEXTEST_VERSION}" "cargo-deny@${CARGO_DENY_VERSION}" \
        "cargo-llvm-cov@${LLVM_COV_VERSION}" "mdbook@${MDBOOK_VERSION}" \
    && chmod -R a+rwX "${CARGO_HOME}"
COPY .devcontainer/hooks/ /opt/devcontainer/hooks/
CMD ["bash"]

FROM base AS builder
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/workspaces/onshape-mcp/target \
    cargo build --release --locked --package onshape-mcp --target x86_64-unknown-linux-musl \
    && install -D target/x86_64-unknown-linux-musl/release/onshape-mcp /out/onshape-mcp

FROM gcr.io/distroless/static-debian13:nonroot AS runtime
COPY --from=builder /out/onshape-mcp /usr/local/bin/onshape-mcp
# UID 1000 matches Clawake's quadlet (UserNS=keep-id:uid=1000, User=1000:1000); the
# private-file checks for the token and approval files depend on it (contract §1, G7).
USER 1000:1000
EXPOSE 8080
# No HEALTHCHECK: distroless has no curl. Clawake keeps the current image until the
# planned `onshape-mcp healthcheck` subcommand exists (contract §1).
ENTRYPOINT ["/usr/local/bin/onshape-mcp"]
CMD ["http"]
