# SPDX-FileCopyrightText: 2026 Karakara Labs
#
# SPDX-License-Identifier: Apache-2.0

# Stage 1: Build the Rust binaries.
#
# Uses stable Rust (matching CI) with lld linker for fast linking.
# Node.js 22 + TypeScript 6.x are required because agent packages
# contain TypeScript sources compiled during the build pipeline.
FROM rust:1.86-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config libssl-dev libdbus-1-dev libcap-ng-dev \
    libclang-dev lld \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && npm install -g typescript@6 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy manifests first for better layer caching.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/
COPY baml_src/host baml_src/host

# Override to stable (rust-toolchain.toml pins nightly for local dev).
ENV RUSTUP_TOOLCHAIN=stable
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=lld"
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=lld"

# Override workspace LTO profile: full LTO needs >8GB RAM during linking.
# Thin LTO produces equivalent binaries within constrained build environments.
ENV CARGO_PROFILE_RELEASE_LTO=thin
ENV CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16

# Optional Cargo parallelism cap (e.g. --build-arg CARGO_BUILD_JOBS=4) for
# memory-constrained macOS/Linux container VMs.
ARG CARGO_BUILD_JOBS

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --release -p karakara-os --features http-tools,memory,sandbox-provider,dev-tools ${CARGO_BUILD_JOBS:+--jobs "$CARGO_BUILD_JOBS"} \
    && cp /build/target/release/karak /usr/local/bin/karak

# Stage 2: Slim runtime image.
#
# Node.js is needed at runtime because repository publish compiles
# TypeScript when agents are published via POST /repository/publish.
FROM debian:bookworm-slim AS runtime

ARG VERSION=dev
LABEL org.opencontainers.image.version="${VERSION}"

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 libdbus-1-3 libcap-ng0 curl \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && npm install -g typescript@6 \
    && rm -rf /var/lib/apt/lists/*

ENV NPM_CONFIG_CACHE=/tmp/npm-cache

RUN groupadd -r -g 1000 karakara && useradd -r -u 1000 -g karakara -d /data -s /sbin/nologin karakara
RUN mkdir -p /data && chown -R karakara:karakara /data

COPY --from=builder /usr/local/bin/karak /usr/local/bin/karak

WORKDIR /data
USER 1000

EXPOSE 18080

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -fsS http://127.0.0.1:18080/healthz || exit 1

ENTRYPOINT ["karak", "serve"]
CMD ["--serve-http", "0.0.0.0:18080"]
