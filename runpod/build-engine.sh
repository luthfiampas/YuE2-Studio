#!/bin/bash
# Builds the pinned yue2.cpp `yue-server` with the CUDA backend for a RunPod pod,
# on this machine, inside an Ubuntu 22.04 CUDA 12.8 container. Ubuntu 22.04 has
# glibc 2.35, so the result also runs on the pod's Ubuntu 24.04 template. Docker
# is only the compiler here: nothing built is an image, and no GPU is needed.
#
#   runpod/build-engine.sh OUTPUT_DIRECTORY [CACHE_DIRECTORY]
#
# Environment:
#   CUDA_ARCH   CUDA architectures to build for. The default is the engine's own list
#               for CUDA 12.8 (Turing to Blackwell) without 121a, which CUDA 12.8 does
#               not know, so one build runs on whichever GPU a pod gets; narrow it only
#               to speed up a local build, e.g. "86-real;89-real". It is recorded in
#               runtime.json.
#   JOBS        parallel compile jobs (default 4: CUDA translation units are
#               heavy, and more of them than memory allows makes the build thrash)
set -euo pipefail

output="${1:?usage: build-engine.sh OUTPUT_DIRECTORY [CACHE_DIRECTORY]}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cache="${2:-${TMPDIR:-/tmp}/yue2-pod-build}"
image="nvidia/cuda:12.8.1-devel-ubuntu22.04"
arch="${CUDA_ARCH:-75-virtual;80-virtual;86-real;89-real;120a-real}"
jobs="${JOBS:-4}"
repository="$(sed -n 's/.*"repository": "\(.*\)".*/\1/p' "$root/engines/yue2-cpp-source.json")"
commit="$(sed -n 's/.*"commit": "\(.*\)".*/\1/p' "$root/engines/yue2-cpp-source.json")"

mkdir -p "$cache" "$output"
output="$(cd "$output" && pwd)"
cache="$(cd "$cache" && pwd)"

source="$cache/yue2-engine"
if [ ! -d "$source/.git" ]; then
    git clone "$repository" "$source"
fi
git -C "$source" fetch --quiet origin "$commit" || git -C "$source" fetch --quiet origin
git -C "$source" checkout --quiet "$commit"
git -C "$source" submodule update --init --recursive

docker run --rm \
    -v "$source:/src" -v "$output:/out" \
    -e ARCH="$arch" -e JOBS="$jobs" -e COMMIT="$commit" -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    "$image" bash -euo pipefail -c '
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -q >/dev/null
    apt-get install -y -q --no-install-recommends ninja-build git ca-certificates curl >/dev/null
    # Ubuntu 22.04 has CMake 3.22, which does not know the "a" in 120a
    curl -sSfL https://github.com/Kitware/CMake/releases/download/v3.31.6/cmake-3.31.6-linux-x86_64.tar.gz | tar -xz -C /opt
    export PATH=/opt/cmake-3.31.6-linux-x86_64/bin:$PATH
    cd /src
    # a configure kept from an earlier run would keep its architectures
    rm -rf build-pod
    cmake -S . -B build-pod -G Ninja -DCMAKE_BUILD_TYPE=Release \
        -DGGML_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES="$ARCH" \
        -DGGML_NATIVE=OFF -DGGML_BACKEND_DL=ON -DGGML_CPU_ALL_VARIANTS=ON \
        -DCMAKE_BUILD_RPATH="\$ORIGIN" -DCMAKE_INSTALL_RPATH="\$ORIGIN" -DCMAKE_BUILD_WITH_INSTALL_RPATH=ON
    # the backends are modules nothing links against, so each is a target of its own
    variants="$(sed -n "s/^build \(ggml-cpu-[a-z0-9_]*\): phony.*/\1/p" build-pod/build.ninja | sort -u | tr "\n" " ")"
    [ -n "$variants" ] || { echo "[ERROR] the configured build has no processor variants of ggml-cpu" >&2; exit 1; }
    cmake --build build-pod --config Release --target yue-server ggml-cuda $variants -j "$JOBS"
    server="$(find build-pod -maxdepth 2 -type f -name yue-server | head -n 1)"
    [ -n "$server" ] || { echo "[ERROR] the build completed without yue-server" >&2; exit 1; }
    cp "$server" /out/
    find build-pod -maxdepth 2 -name "lib*.so*" -exec cp -a {} /out/ \;
    printf "{\"commit\":\"%s\",\"backend\":\"cuda\",\"platform\":\"linux\",\"cuda_arch\":\"%s\"}\n" "$COMMIT" "$ARCH" > /out/runtime.json
    chown -R "$HOST_UID:$HOST_GID" /out /src/build-pod
'
echo "yue-server ($commit, CUDA $arch) built into $output"
