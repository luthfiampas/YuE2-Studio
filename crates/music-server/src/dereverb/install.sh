#!/bin/bash
# Installs the de-reverb model and its loader into the shared Python
# environment. The environment itself (Python + torch) is the same one voice
# swap uses, installed from Settings; this only adds what the model needs.
#
#   install.sh ROOT
#
# Environment:
#   DEREVERB_VENV  the shared environment (…/data/python/venv)
set -euo pipefail

root="$(mkdir -p "${1:-.}" && cd "${1:-.}" && pwd)"
venv="${DEREVERB_VENV:-${root}/venv}"

emit() { printf '{"event":"stage","stage":"%s","progress":%s}\n' "$1" "$2"; }
log() { printf '{"event":"log","line":"%s"}\n' "$1"; }
die() { printf '{"event":"error","message":"%s"}\n' "$1"; exit 1; }

if [ ! -x "${venv}/bin/python" ]; then
    die "the Python runtime is not installed; set it up under Settings, Python runtime"
fi

# A half-finished attempt must not count as installed.
rm -f "${root}/.ready"

emit "Installing the loader" 0.2
if ! "${venv}/bin/python" -c 'import onnxruntime' >/dev/null 2>&1; then
    "${venv}/bin/python" -m pip install --no-input --progress-bar off onnxruntime >&2 \
        || die "onnxruntime could not be installed"
fi
# audio-separator declares numpy>=2; RVC needs numpy<2. The 0.47 build works on
# numpy 1.26 (verified), so it is installed with --no-deps and its own
# dependencies are listed here. Letting pip resolve would silently pick 0.30,
# whose API differs.
"${venv}/bin/python" -m pip install --no-input --progress-bar off \
    "numpy<2" "scipy<1.15" "librosa<0.11" soundfile "samplerate==0.1.0" resampy pydub audioread >&2 \
    || die "the shared packages could not be installed"
"${venv}/bin/python" -m pip install --no-input --progress-bar off --no-deps "audio-separator==0.47.1" >&2 \
    || die "audio-separator could not be installed"
constraints="${root}/constraints.txt"
printf 'numpy<2\nscipy<1.15\nlibrosa<0.11\nrotary-embedding-torch<0.7.0\ntorch<2.8\ntorchaudio<2.8\ntorchvision<0.23\n' > "$constraints"
"${venv}/bin/python" -m pip install --no-input --progress-bar off -c "$constraints" \
    beartype diffq einops julius ml_collections onnx onnx2torch "rotary-embedding-torch<0.7.0" \
    pyyaml requests six tqdm packaging "torchvision==0.22.1" >&2 \
    || die "audio-separator's dependencies could not be installed"
# onnx pulls numpy 2 as a dependency; the loader runs on 1.26 and RVC requires
# it, so the pins are applied once more after the resolver has had its say.
"${venv}/bin/python" -m pip install --no-input --progress-bar off "numpy<2" "scipy<1.15" "librosa<0.11" "rotary-embedding-torch<0.7.0" >&2 \
    || die "the shared pins could not be restored"

model="${root}/models/UVR-DeEcho-DeReverb.pth"
url="https://github.com/TRvlvr/model_repo/releases/download/all_public_uvr_models/UVR-DeEcho-DeReverb.pth"
bytes=223650277
sha="e644028ec82865dc0fe082bc6fea85a43f7c71cfe375caee2da2d154aa661ee7"

if [ -f "${model}" ] && [ "$(stat -c %s "${model}")" = "${bytes}" ] \
    && [ "$(sha256sum "${model}" | cut -d' ' -f1)" = "${sha}" ]; then
    log "the model is already here"
else
    emit "Downloading the model" 0.5
    mkdir -p "${root}/models"
    curl -fL --retry 3 --retry-delay 2 -o "${model}.part" "${url}" >&2 || die "the model could not be downloaded"
    [ "$(stat -c %s "${model}.part")" = "${bytes}" ] || { rm -f "${model}.part"; die "the model came back the wrong size"; }
    [ "$(sha256sum "${model}.part" | cut -d' ' -f1)" = "${sha}" ] || { rm -f "${model}.part"; die "the model did not match its published hash"; }
    mv "${model}.part" "${model}"
fi

# The shared environment must still be RVC's: adding the loader must not have
# moved torch or stopped it loading.
emit "Checking the shared environment" 0.8
"${venv}/bin/python" -c 'import torch, audio_separator' >&2 || die "the shared Python environment no longer loads; reinstall it from Settings"
torch_version="$("${venv}/bin/python" -c 'import torch; print(torch.__version__)' 2>/dev/null)"
case "$torch_version" in
    2.7.1*) ;;
    *) die "torch is now ${torch_version:-missing}; voice swap needs 2.7.1. Reinstall the Python runtime from Settings" ;;
esac
log "torch ${torch_version} still loads"

touch "${root}/.ready"

emit "Done" 1.0
printf '{"event":"done"}\n'
