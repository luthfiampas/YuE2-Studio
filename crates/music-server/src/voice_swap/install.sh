#!/bin/bash
# Installs the voice swap engine under ROOT: a virtual environment with torch,
# RVC at the pinned commit, and the four weight files. Prints the runner's own
# JSON protocol, so the studio shows one progress bar for the whole thing.
#
#   install.sh ROOT
#
# Environment:
#   VOICE_RVC_REPOSITORY, VOICE_RVC_COMMIT, VOICE_RVC_REQUIREMENTS
#                    the pin, passed by the server from engines/rvc-source.json
#   VOICE_PYTHON     interpreter to build the environment from (python3)
#   VOICE_TORCH      torch and torchaudio version (2.7.1+cu128)
#   VOICE_TORCH_INDEX  the index those wheels come from
#                    (https://download.pytorch.org/whl/cu128)
#   VOICE_SKIP_TORCH 1 to leave the environment's torch alone: a machine where a
#                    CPU build is already installed is testing the rest for free
#
# A pinned commit, not a branch: the requirements are read out of the checkout,
# so what they install is fixed with it.

set -euo pipefail

root="$(mkdir -p "${1:-.}" && cd "${1:-.}" && pwd)"
# The environment is shared by every Python tool and lives beside them; the
# server passes its absolute path.
venv="${VOICE_VENV:-${root}/venv}"

python="${VOICE_PYTHON:-python3}"
torch="${VOICE_TORCH:-2.7.1+cu128}"
torch_index="${VOICE_TORCH_INDEX:-https://download.pytorch.org/whl/cu128}"
skip_torch="${VOICE_SKIP_TORCH:-0}"

repository="${VOICE_RVC_REPOSITORY:-}"
commit="${VOICE_RVC_COMMIT:-}"
requirements="${VOICE_RVC_REQUIREMENTS:-requirments_cu128_py312.txt}"
if [ -z "$repository" ] || [ -z "$commit" ]; then
    printf '{"event":"error","message":"the RVC pin was not passed to the installer"}\n'
    exit 1
fi

# The weights, at the revision they were measured at. The hash is checked: a
# file that differs is removed instead of being used.
asset_host="https://huggingface.co/lj1995/VoiceConversionWebUI/resolve"
asset_revision="e6d0c1a17da07c33557852f9dfa2bd44cc75737d"

emit() { printf '{"event":"stage","stage":"%s","progress":%s}\n' "$1" "$2"; }
log() { printf '{"event":"log","line":"%s"}\n' "$1"; }
die() { printf '{"event":"error","message":"%s"}\n' "$1"; exit 1; }

fetch() {
    local name="$1" path="$2" bytes="$3" sha="$4"
    local url="${asset_host}/${asset_revision}/${name}"
    if [ -f "$path" ] && [ "$(stat -c %s "$path" 2>/dev/null || echo 0)" = "$bytes" ] \
        && command -v sha256sum >/dev/null 2>&1 \
        && [ "$(sha256sum "$path" | cut -d' ' -f1)" = "$sha" ]; then
        return 0
    fi
    mkdir -p "$(dirname "$path")"
    curl -fL --retry 3 --retry-delay 2 -o "${path}.part" "$url" >/dev/null 2>&1 \
        || die "could not download ${name}"
    if [ "$(stat -c %s "${path}.part")" != "$bytes" ]; then
        rm -f "${path}.part"
        die "${name} came back the wrong size"
    fi
    if command -v sha256sum >/dev/null 2>&1; then
        local actual
        actual="$(sha256sum "${path}.part" | cut -d' ' -f1)"
        if [ "$actual" != "$sha" ]; then
            rm -f "${path}.part"
            die "${name} did not match its published hash"
        fi
    fi
    mv "${path}.part" "$path"
}

# torch's CUDA wheels, the installed copy and the libraries they pull need
# several gigabytes; a full disk halfway through leaves a half-written
# environment that looks installed. Said before it starts, not after.
space_gb="${VOICE_SPACE_GB:-}"
if [ -z "$space_gb" ]; then
    case "$torch_index" in
        *cpu*) space_gb=3 ;;
        *) space_gb=10 ;;
    esac
fi
available_kb="$(df -Pk "$root" 2>/dev/null | awk 'NR==2 {print $4}')"
if [ -n "$available_kb" ] && [ "$available_kb" -lt $((space_gb * 1024 * 1024)) ]; then
    die "needs about ${space_gb} GB free at ${root}, and has $((available_kb / 1024 / 1024)) GB"
fi

if ! command -v "$python" >/dev/null 2>&1; then
    die "no ${python} on the server: the pod image carries one"
fi
if ! "$python" -c 'import sys; sys.exit(0 if sys.version_info[:2] == (3, 12) else 1)' >/dev/null 2>&1; then
    die "RVC's pin needs Python 3.12; ${python} is $("$python" -V 2>&1 | cut -d' ' -f2)"
fi
if ! command -v ffmpeg >/dev/null 2>&1; then
    die "ffmpeg is not on the server: RVC decodes recordings with it"
fi

emit "Preparing the environment" 0.02
if [ ! -x "${venv}/bin/python" ]; then
    "$python" -m venv "${venv}" >/dev/null 2>&1 || die "the virtual environment could not be created"
fi
venv_python="${venv}/bin/python"
"${venv_python}" -m pip install --quiet --progress-bar off --upgrade pip setuptools wheel >&2 \
    || die "pip could not be updated"

if [ "$skip_torch" != "1" ]; then
    emit "Installing torch" 0.25
    log "torch ${torch}"
    "${venv_python}" -m pip install --no-input --progress-bar off \
        "torch==${torch}" "torchaudio==${torch}" \
        --index-url "$torch_index" --extra-index-url https://pypi.org/simple >&2 \
        || die "torch ${torch} could not be installed"
fi

# The runtime is the shared half: Python and torch, which every Python tool
# uses. A tool's own checkout and weights are a second step.
if [ "${VOICE_RUNTIME_ONLY:-0}" = "1" ]; then
    mkdir -p "${root}/rvc"
    # The packages every Python tool loads, not just torch: a de-reverb run
    # needs numpy, librosa, soundfile and an onnxruntime too.
    emit "Installing the shared packages" 0.5
    "${venv_python}" -m pip install --no-input --progress-bar off "numpy<2" "scipy<1.15" "librosa<0.11" soundfile "samplerate==0.1.0" resampy pydub audioread >&2 \
        || die "the shared packages could not be installed"
    if ! "${venv_python}" -c 'import onnxruntime' >/dev/null 2>&1; then
        "${venv_python}" -m pip install --no-input --progress-bar off onnxruntime >&2 \
            || die "onnxruntime could not be installed"
    fi
    touch "${venv}/.runtime-ready"
    emit "Done" 1.0
    printf '{"event":"done"}\n'
    exit 0
fi

emit "Fetching RVC" 0.55
if [ ! -d "${root}/rvc/.git" ]; then
    git clone --filter=blob:none --no-checkout "$repository" "${root}/rvc" >&2 \
        || die "RVC could not be cloned"
fi
git -C "${root}/rvc" fetch --depth 1 origin "$commit" >&2 || die "RVC commit ${commit:0:8} could not be fetched"
git -C "${root}/rvc" checkout --detach "$commit" >&2 || die "RVC commit ${commit:0:8} could not be checked out"
log "RVC ${commit:0:8}"

emit "Installing its dependencies" 0.7
# Upstream's file names a mirror this network cannot always reach; pip's
# --index-url is overridden by a line inside the file, so the file is copied
# without its index lines and pointed at PyPI here.
sed '/^--index-url/d' "${root}/rvc/${requirements}" > "${root}/rvc/requirements.local.txt"
"${venv_python}" -m pip install --no-input --progress-bar off -r "${root}/rvc/requirements.local.txt" \
    --index-url https://pypi.org/simple >&2 \
    || die "RVC's dependencies could not be installed"
rm -f "${root}/rvc/requirements.local.txt"

emit "Fetching the weights" 0.85
fetch "hubert_base/config.json" "${root}/rvc/assets/hubert_base/config.json" 1492 0346950779dfb7f9316fa74ed846e2b8a22a08eedfdc5387b73f327cb1a4a7cf
fetch "hubert_base/preprocessor_config.json" "${root}/rvc/assets/hubert_base/preprocessor_config.json" 225 7c1976a680fb7acc757cd36fb08eef878fa36c70b4c9d2d595df9c608bbbbf0e
fetch "hubert_base/pytorch_model.bin" "${root}/rvc/assets/hubert_base/pytorch_model.bin" 189206711 cc8c20f4b90a520757260197a3ff2505705a7adbd20ad9eeaa4e1a9b38442ef5
fetch "rmvpe.pt" "${root}/rvc/assets/rmvpe/rmvpe.pt" 181184272 6d62215f4306e3ca278246188607209f09af3dc77ed4232efdd069798c4ec193
fetch "pretrained_v2/f0G48k.pth" "${root}/rvc/assets/pretrained_v2/f0G48k.pth" 75465569 b5d51f589cc3632d4eae36a315b4179397695042edc01d15312e1bddc2b764a4
fetch "pretrained_v2/f0D48k.pth" "${root}/rvc/assets/pretrained_v2/f0D48k.pth" 142875703 2269b73c7a4cf34da09aea99274dabf99b2ddb8a42cbfb065fb3c0aa9a2fc748

mkdir -p "${root}/rvc/assets/weights" "${root}/rvc/assets/indices"
touch "${venv}/.runtime-ready"
# The engine is ready when the last file is in place, not when the checkout is.
touch "${root}/.ready"

emit "Done" 1.0
printf '{"event":"done"}\n'
