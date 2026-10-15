# syntax=docker/dockerfile:1
#
# Scratch's base image: what every session's sandbox runs on, read-only, under
# the session's own layer. Exported to a plain directory by export-rootfs.sh
# and handed to Faber as FABER_SCRATCH_ROOTFS; Faber never runs it as a
# container.
#
# Virtual mode reads the same directory, so whatever is installed here is
# what `command -v`, `cat /etc/os-release` and PATH lookups answer in both
# modes.
#
# The package list is fixed: the tools every session can count on, plus the
# installers and shims for toolchains (uv, rustup, mise) — never the
# toolchains themselves. Those live in the toolstore, bound read-only at
# /opt/toolstore.

FROM debian:trixie-slim

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        fd-find \
        file \
        git \
        jq \
        less \
        libssl-dev \
        openssh-client \
        pkg-config \
        procps \
        python3 \
        ripgrep \
        rustup \
        tree \
        unzip \
        xz-utils \
        zstd \
    && rm -rf /var/lib/apt/lists/*

# uv and mise are not packaged by Debian: their upstream release binaries,
# pinned and checked, installed as single executables with no toolchain.
ARG UV_VERSION=0.8.0
ARG MISE_VERSION=2025.7.0
RUN set -eu; \
    arch=$(dpkg --print-architecture); \
    case "$arch" in \
        amd64) uv_target=x86_64-unknown-linux-gnu; \
               uv_sha=a7d74ee5c5ff3069b9d88236a05f293cc4e2809bad872f3a88a384489ba3675e; \
               mise_arch=x64; \
               mise_sha=684b0eb43934913fb9246d3f9964f416d53a35c991aaf2d8cc112a41404888a7 ;; \
        arm64) uv_target=aarch64-unknown-linux-gnu; \
               uv_sha=61f136a187150624c10ec7025137fdf5391ef083a4cab0eba4fbdb2a928de16b; \
               mise_arch=arm64; \
               mise_sha=741a12aee10a331a0a17d9dec81f99c9d17d891c59952d6d72c5186e28eb4134 ;; \
        *) echo "no uv/mise pinned for $arch" >&2; exit 1 ;; \
    esac; \
    cd /tmp; \
    curl -fsSLo uv.tar.gz "https://github.com/astral-sh/uv/releases/download/${UV_VERSION}/uv-${uv_target}.tar.gz"; \
    echo "${uv_sha}  uv.tar.gz" | sha256sum -c -; \
    tar -xzf uv.tar.gz; \
    install -m 0755 "uv-${uv_target}/uv" "uv-${uv_target}/uvx" /usr/local/bin/; \
    curl -fsSLo mise "https://github.com/jdx/mise/releases/download/v${MISE_VERSION}/mise-v${MISE_VERSION}-linux-${mise_arch}"; \
    echo "${mise_sha}  mise" | sha256sum -c -; \
    install -m 0755 mise /usr/local/bin/mise; \
    rm -rf /tmp/uv.tar.gz "/tmp/uv-${uv_target}" /tmp/mise

# gVisor mounts a tmpfs over an empty /tmp, which would drop whatever a
# session keeps there at idle teardown. A file in it keeps /tmp on the
# overlay, where virtual mode has it too.
RUN touch /tmp/.keep

# The toolstore is bind-mounted read-only here in both modes.
RUN mkdir -p /opt/toolstore /mnt

WORKDIR /root
