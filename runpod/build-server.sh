#!/bin/bash
# Builds the headless music-server binary for Linux deployment.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST_BIN="${ROOT}/runpod/dist/bin"
mkdir -p "$DIST_BIN"

echo "=========================================="
echo " Building headless music-server"
echo "=========================================="

CACHE_DIR="${ROOT}/../build-cache"
mkdir -p "${CACHE_DIR}/cargo" "${CACHE_DIR}/rustup"

if command -v docker >/dev/null 2>&1; then
    docker run --rm \
        -v "$ROOT:/src" \
        -v "${CACHE_DIR}/cargo:/root/.cargo" \
        -v "${CACHE_DIR}/rustup:/root/.rustup" \
        -w /src \
        nvidia/cuda:12.8.1-devel-ubuntu22.04 bash -euo pipefail -c '
        export DEBIAN_FRONTEND=noninteractive RUSTUP_TOOLCHAIN=stable
        apt-get update -q >/dev/null
        apt-get install -y -q --no-install-recommends pkg-config libssl-dev libasound2-dev >/dev/null
        source /root/.cargo/env 2>/dev/null || true
        if ! command -v cargo >/dev/null 2>&1; then
            curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
            source /root/.cargo/env
        fi
        cargo +stable build --release -p music-server
        cp target/release/music-server /src/runpod/dist/bin/
    '
elif command -v cargo >/dev/null 2>&1; then
    echo "[..] Docker not found; compiling with host cargo..."
    cargo build --release -p music-server
    cp "${ROOT}/target/release/music-server" "${DIST_BIN}/"
else
    echo "[ERROR] Neither Docker nor Cargo is installed. Cannot build music-server." >&2
    exit 1
fi

echo "=========================================="
echo "[ok] Headless music-server ready: ${DIST_BIN}/music-server"
echo "=========================================="
