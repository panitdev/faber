# syntax=docker/dockerfile:1

FROM rust:bookworm AS chef

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends libpq-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-chef --locked

# planner
FROM chef AS planner

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo chef prepare --recipe-path recipe.json

# builder
FROM chef AS builder

COPY --from=planner /app/recipe.json recipe.json

RUN --mount=type=cache,id=faber-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=faber-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=faber-api-target,target=/app/target,sharing=locked \
    cargo chef cook --release --locked -p api --recipe-path recipe.json

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN --mount=type=cache,id=faber-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=faber-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=faber-api-target,target=/app/target,sharing=locked \
    cargo build --release --locked -p api \
    && mkdir -p /out \
    && cp target/release/api /out/faber-api
# The agent daemon the API serves to enrolling hosts, built here so the
# served binary and the running server are always the same release.
#
# Its own target, its own cache scope, and its own chef cook: this is a
# static musl build, which shares no compiled artifact with the glibc API
# build above. A binary built against this image's glibc 2.36 would fail on
# Debian 11 or Ubuntu 20.04 with `GLIBC_2.34 not found` — and the whole point
# of agent transport is hosts whose libc faber does not get to choose.
FROM chef AS agent-cacher

RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-unknown-linux-musl

ENV CC_x86_64_unknown_linux_musl=musl-gcc
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc
# `crt-static` alone yields a static-*pie*, which still asks for
# /lib/ld-musl-x86_64.so.1 at exec and so dies on a glibc host with a bare
# "no such file or directory". Pinning the relocation model is what makes it
# a genuinely self-contained executable. Verified by running the result
# under debian:bookworm-slim.
ENV RUSTFLAGS="-C target-feature=+crt-static -C relocation-model=static"

COPY --from=planner /app/recipe.json recipe.json

# Scoped to this one crate: cooking the whole workspace recipe would pull the
# API's libpq-linked dependency tree into a musl build that has no libpq and
# no need of one.
RUN --mount=type=cache,id=faber-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=faber-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=faber-agent-target,target=/app/target,sharing=locked \
    cargo chef cook --release --locked --target x86_64-unknown-linux-musl \
        -p faber-agent --recipe-path recipe.json

FROM agent-cacher AS agent-builder

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN --mount=type=cache,id=faber-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=faber-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=faber-agent-target,target=/app/target,sharing=locked \
    cargo build --release --locked --target x86_64-unknown-linux-musl -p faber-agent \
    && mkdir -p /out \
    && cp target/x86_64-unknown-linux-musl/release/faber-agent /out/faber-agent-x86_64 \
    && strip /out/faber-agent-x86_64

FROM debian:bookworm-slim AS runner

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libpq5 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /usr/sbin/nologin faber

WORKDIR /app

ENV API_PORT=3001
ENV FABER_AGENT_BINARY_DIR=/usr/local/share/faber/agent

COPY --from=builder /out/faber-api /usr/local/bin/faber-api
# Served by GET /api/agent/binary/{arch}; one file per architecture, named
# for what `uname -m` reports on the target.
COPY --from=agent-builder /out/faber-agent-x86_64 ${FABER_AGENT_BINARY_DIR}/faber-agent-x86_64

USER faber

EXPOSE 3001

CMD ["faber-api"]
