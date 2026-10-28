#!/bin/bash
# Packages the engine and server binaries into a deployable archive for RunPod.
# No runner scripts are included in this archive; the launcher streams the runner directly over SSH.
#
# Nothing here deletes the previous archive before it has a verified replacement:
# a failed build used to leave last week's tarball behind for the launcher to
# push, and the pod then ran old binaries with no sign of it.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST_DIR="${ROOT}/runpod/dist"
BUNDLE_DIR="${DIST_DIR}/bundle"
STAGING="${DIST_DIR}/bundle.new"
ARCHIVE="${DIST_DIR}/yue2-studio-runpod.tar.gz"
SERVER="${DIST_DIR}/bin/music-server"
ENGINE_SRC="${ROOT}/../build-cache/out-engine-all"

fail() { echo "[ERROR] $*" >&2; exit 1; }

echo "[..] Checking music-server..."
if [ ! -f "$SERVER" ]; then
    echo "[..] ${SERVER} not found. Auto-building headless server..."
    "${ROOT}/runpod/build-server.sh"
fi
[ -f "$SERVER" ] || fail "music-server was not built"

echo "[..] Checking engine binaries..."
if [ ! -f "${ENGINE_SRC}/yue-server" ]; then
    echo "[..] ${ENGINE_SRC}/yue-server not found. Auto-building CUDA engine..."
    "${ROOT}/runpod/build-engine.sh" "$ENGINE_SRC"
fi
[ -f "${ENGINE_SRC}/yue-server" ] || fail "yue-server was not built"

# Staged, so a copy that fails halfway cannot leave a bundle that looks ready.
rm -rf "$STAGING"
mkdir -p "${STAGING}/bin"
cp -a "${ENGINE_SRC}/." "${STAGING}/bin/"
cp -a "$SERVER" "${STAGING}/bin/"
[ -f "${STAGING}/bin/music-server" ] || fail "music-server did not reach the bundle"
[ -f "${STAGING}/bin/yue-server" ] || fail "yue-server did not reach the bundle"

# The archive is written beside the old one and renamed over it at the end: a
# failure leaves the old archive, and the launcher refuses to push it.
tar -czf "${ARCHIVE}.new" -C "$STAGING" bin || fail "the archive could not be written"
[ -s "${ARCHIVE}.new" ] || fail "the archive is empty"
mv "${ARCHIVE}.new" "$ARCHIVE"

rm -rf "$BUNDLE_DIR"
mv "$STAGING" "$BUNDLE_DIR"

SIZE="$(du -h "$ARCHIVE" | cut -f1)"
echo "=========================================="
echo "[ok] Package created: ${ARCHIVE} (${SIZE}) [Binaries only]"
echo "=========================================="
