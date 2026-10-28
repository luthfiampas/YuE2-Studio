#!/bin/bash
# Streamed directly over SSH by the launcher. No scripts are stored on the pod.
set -euo pipefail

WORKSPACE="${WORKSPACE:-/workspace}"
# Keep in sync with REMOTE_STUDIO_DIR in crates/launcher/src/ssh.rs.
STUDIO_DIR="${WORKSPACE}/yue2-studio"
BIN_DIR="${STUDIO_DIR}/bin"
DATA_DIR="${STUDIO_DIR}/data"
MEDIA_DIR="${STUDIO_DIR}/media"
MODELS_DIR="${STUDIO_DIR}/models"

# 1. Ensure studio directories exist
mkdir -p "$BIN_DIR" "$DATA_DIR" "$MEDIA_DIR" "$MODELS_DIR"

# 2. Check binaries exist
if [ ! -f "${BIN_DIR}/music-server" ] || [ ! -f "${BIN_DIR}/yue-server" ]; then
    echo "[ERROR] Binaries not found in ${BIN_DIR}. Push them from the launcher with 'p' first." >&2
    exit 1
fi

chmod +x "${BIN_DIR}/music-server" "${BIN_DIR}/yue-server"

# 3. Stop any existing server processes cleanly
pkill -9 -x "music-server" 2>/dev/null || true
pkill -9 -x "yue-server" 2>/dev/null || true
sleep 1

# 4. Configure Environment
export LD_LIBRARY_PATH="${BIN_DIR}:${LD_LIBRARY_PATH:-}"
PYTORCH_LIBS="$(python3 -c 'import torch, os; print(os.path.join(os.path.dirname(torch.__file__), "lib"))' 2>/dev/null || true)"
if [ -n "$PYTORCH_LIBS" ] && [ -d "$PYTORCH_LIBS" ]; then
    export LD_LIBRARY_PATH="${PYTORCH_LIBS}:${LD_LIBRARY_PATH}"
fi
export PATH="${BIN_DIR}:${PATH}"

export YUE_STUDIO_DATA_ROOT="$DATA_DIR"
export YUE_MODELS_ROOT="$MODELS_DIR"
export YUE_ENGINE_ROOT="$BIN_DIR"
export YUE_ENGINE_BIN="${BIN_DIR}/yue-server"
export YUE_STUDIO_PORT="8791"

# 5. Start music-server in background
nohup "${BIN_DIR}/music-server" > "${STUDIO_DIR}/yue2-studio.log" 2>&1 </dev/null &
PID=$!
disown -a 2>/dev/null || true
echo "[ok] YuE2 Studio remote daemon started (PID: $PID)."
