# YuE2 Studio

A fork of [timoncool/YuE2-Studio](https://github.com/timoncool/YuE2-Studio). Upstream is a desktop application needing a local NVIDIA GPU with 6 GB to 12+ GB of VRAM. Here generation runs on a headless server on [RunPod](https://runpod.io) and the client is a browser tab.

## Requirements

- Linux or macOS with `ssh` and `scp`.
- Node.js 22 and npm.
- Rust stable. `rust-toolchain.toml` pins the channel.
- Docker for the first build. It compiles the server and the engine in a container, so no local GPU is involved.
- A RunPod account with an API key and a persistent Network Volume.

## Quickstart

```bash
cargo run --release -p launcher
```

1. `n` creates a pod. The network volume you pick fixes its region and mounts at `/workspace`.
2. `b` and `e` build the server and the engine, once per checkout. `p` uploads them.
3. `Enter` opens the tunnel, `w` starts the web dev server and `o` opens `http://localhost:3791`.

Press `?` in the launcher for the other keys.

## How it works

The tunnel forwards a local port to `127.0.0.1:8791` on the pod, encrypted over RunPod's SSH port. The local port defaults to 8791 and `c` changes it. No web port is exposed.

Weights (15+ GB), audio caches and your library stay on the volume under `/workspace`. The bottom bar shows balance, VRAM, CPU and RAM, and `x` stops the pod to end GPU billing.

## Python runtime

**Settings → Python runtime** installs the environment the Python tools share: Python 3.12 and torch, about 8 GB and ten to twenty minutes on the network volume, once. It is not part of any one tool - a tool adds only its own loader and model on top - and a pod restart never reinstalls it.

## Misc

The tools that are not part of making a song live under **Misc** in the sidebar: they get their own list, their own history and no say in how a song is made. Voice swap and De-reverb are there now; the next tool goes beside them.

### Voice swap

Trains a voice from your own recordings and sings a vocal stem in it. Recordings are a draft until training has made a model; once a run succeeds they are deleted, and the voice keeps the minutes it was trained on along with one 55 MB model per saved checkpoint. A long run leaves many; **Clean up** keeps the ones you tick and deletes the rest. A swap takes an uploaded stem or a vocal stem already separated in the library, and writes a WAV into the tool's own history - no song is created or changed.

Everything lives under `/workspace/yue2-studio/data/voice-swap`, so models and results survive the pod. Training needs an NVIDIA card; a conversion can fall back to the processor. The pinned RVC commit is in `engines/rvc-source.json`.

### De-reverb

One model, `UVR-DeEcho-DeReverb` (214 MB): a vocal stem in, the dry take and the reverb that was removed out, both kept beside the run in the history. It takes out the room a generator baked into a vocal - the part a separator cannot help with, and which a voice model otherwise learns.

It runs through `audio-separator` in the shared Python environment, so it adds a loader and a model rather than another interpreter. On a Suno vocal stem it removed the most of the models tried; `MDX23C-De-Reverb` less, and `Reverb HQ` and the MelBand RoFormer dereverb did nothing. None of them makes a wet vocal dry, so a dry recording still beats any of them.

## Building the binaries

Nothing ships these prebuilt, so the first deployment compiles them: `b` and `e` run `runpod/build-server.sh` and `runpod/build-engine.sh`. Afterwards they live in `/workspace/yue2-studio/bin` on the volume and later sessions only reconnect.

The engine build is the slow one. It clones the commit pinned in `engines/yue2-cpp-source.json` and compiles for every architecture from Turing to Blackwell unless you narrow `CUDA_ARCH`. `JOBS` (default 4) sets parallel compile jobs.

Without Docker, the server script falls back to a host `cargo build`, which only produces something the pod can run when the host is Linux x86-64.
