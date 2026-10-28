#!/usr/bin/env python3
"""Voice swap runner.

music-server starts one of these per job, so the graphics card is held only
while work happens. It writes one JSON object per line to stdout:

    {"event": "stage", "stage": "Training", "progress": 0.7, "detail": "2/20"}
    {"event": "log", "line": "..."}
    {"event": "done", "output": "/path/to/result.wav"}
    {"event": "error", "message": "..."}

Commands:

    runner.py selftest --root DIR
    runner.py train    --root DIR --job FILE
    runner.py convert  --root DIR --job FILE

`root` is the voice-swap folder: `rvc/` is the pinned checkout, `venv/` the
interpreter this runs under. Nothing heavy is imported for a command that does
not need it, so a self-test answers on a machine where RVC is half installed.

The training job:

    {"name": "my-voice", "recordings": "/…/voices/v1/recordings",
     "output": "/…/voices/v1", "epochs": 20, "save_every": 5,
     "batch": 4, "workers": 2}

The conversion job:

    {"model": "/…/model-e20.pth", "index": "/…/model.index",
     "source": "/…/source.wav", "output": "/…/output.wav",
     "pitch": -7, "index_rate": 0.75, "protect": 0.33, "rms_mix_rate": 0.25}
"""

import argparse
import json
import os

# Before RVC imports torch: its CUDA-graph cache captures a graph per input
# shape and keeps the private pool, which fills a 16 GB card during a single
# conversion. Upstream's UI leaves graphs off by default for this reason.
os.environ.setdefault("RVC_CUDA_GRAPH", "0")
os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")
import re
import shutil
import subprocess
import sys
from pathlib import Path


def emit(**event):
    sys.stdout.write(json.dumps(event, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def stage(name, progress=None, detail=None):
    emit(event="stage", stage=name, progress=progress, detail=detail)


def fail(message):
    emit(event="error", message=message)
    return 1


# Name, size and the answer it gives. The sizes are the pinned downloads'; a
# file of the wrong size is a cut transmission, not a weight.
WEIGHTS = {
    "assets/hubert_base/config.json": 1_492,
    "assets/hubert_base/preprocessor_config.json": 225,
    "assets/hubert_base/pytorch_model.bin": 189_206_711,
    "assets/rmvpe/rmvpe.pt": 181_184_272,
    "assets/pretrained_v2/f0G48k.pth": 75_465_569,
    "assets/pretrained_v2/f0D48k.pth": 142_875_703,
}


def asset_paths(rvc: Path):
    """Each weight, where it goes, and how big it must be."""
    return {name: (rvc / name, size) for name, size in WEIGHTS.items()}


def selftest(root: Path) -> int:
    stage("Python", 0.1, sys.version.split()[0])

    stage("Checking the environment", 0.3)
    try:
        import torch
    except Exception as error:  # noqa: BLE001 - the message is the report
        return fail("torch does not load in this environment: %s" % error)
    emit(event="log", line="torch %s" % torch.__version__)

    if not torch.cuda.is_available():
        emit(event="log", line="no CUDA device: conversions will run on the processor")

    import shutil as _shutil

    if not _shutil.which("ffmpeg"):
        return fail("ffmpeg is not on this server: RVC decodes recordings with it")

    rvc = root / "rvc"
    stage("Checking RVC", 0.6)
    required = [rvc / "infer" / "cli.py", rvc / "configs" / "config.py", rvc / "train" / "train.py"]
    missing = [str(path.relative_to(root)) for path in required if not path.is_file()]
    if missing:
        return fail("RVC is not installed correctly: missing %s" % ", ".join(missing))

    absent = []
    for name, (path, size) in asset_paths(rvc).items():
        short = name.rsplit("/", 1)[-1]
        if not path.is_file():
            absent.append(short)
        elif path.stat().st_size != size:
            absent.append("%s (wrong size)" % short)
    if absent:
        return fail("missing or incomplete weights: %s" % ", ".join(absent))

    stage("Ready", 1.0, "torch %s" % torch.__version__)
    emit(event="done")
    return 0


def run(stage_name, progress, arguments, cwd: Path, environment=None):
    """Runs one of RVC's scripts, passing its output to the studio's log."""
    stage(stage_name, progress)
    process = subprocess.Popen(
        arguments,
        cwd=str(cwd),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        env={**os.environ, **environment} if environment else None,
    )
    assert process.stdout is not None
    for line in process.stdout:
        line = line.rstrip()
        if line:
            emit(event="log", line=line)
    code = process.wait()
    if code != 0:
        raise RuntimeError("%s exited with code %d" % (stage_name, code))


def train(root: Path, job) -> int:
    rvc = root / "rvc"
    name = job["name"]
    recordings = Path(job["recordings"])
    output = Path(job["output"])
    epochs = int(job.get("epochs", 20))
    save_every = int(job.get("save_every", 5))
    # 0 from the server means "size it for the card this job computes on".
    batch = int(job.get("batch") or 0) or batch_for_card()
    workers = int(job.get("workers", 2))
    python = sys.executable
    os.environ["RVC_CUDA_GRAPH"] = "0"
    os.environ["PYTHONUNBUFFERED"] = "1"
    experiment = rvc / "logs" / name
    experiment.mkdir(parents=True, exist_ok=True)
    output.mkdir(parents=True, exist_ok=True)
    for stale in list(experiment.iterdir()):
        if stale.name in ("config.json", "preprocess.log", "extract_f0_feature.log", "train_index.log", "train.log"):
            continue
        shutil.rmtree(stale) if stale.is_dir() else stale.unlink(missing_ok=True)
    (rvc / "assets" / "weights" / ("%s.pth" % name)).unlink(missing_ok=True)
    for stale in (rvc / "assets" / "weights").glob("%s_*.pth" % name):
        stale.unlink(missing_ok=True)
    # The trainer reads this, and only the web UI ever writes it.
    shutil.copyfile(rvc / "configs" / "v2" / "48k.json", experiment / "config.json")
    # `savee()` writes here and swallows a missing directory, so a run without
    # it finishes every epoch and saves nothing.
    (rvc / "assets" / "weights").mkdir(parents=True, exist_ok=True)



    try:
        run(
            "Splitting the recordings",
            0.08,
            [python, "-m", "train.preprocess", str(recordings), "48000", str(workers), str(experiment), "False", "3.7"],
            rvc,
        )

        cuda = safe_cuda()
        if not cuda and os.environ.get("VOICE_ALLOW_CPU_TRAIN") != "1":
            return fail("training needs an NVIDIA graphics card; conversions can run on the processor")
        if cuda:
            half = "True" if half_precision() else "False"
            run(
                "Reading pitch",
                0.32,
                [python, "-m", "train.dataset.extract_f0", "cuda", "1", "0", "0", str(experiment), half],
                rvc,
            )
            run(
                "Reading timbre",
                0.55,
                [python, "-m", "train.dataset.extract_hubert_feature", "cuda", "1", "0", str(experiment), "v2", half],
                rvc,
            )
        else:
            run(
                "Reading pitch",
                0.32,
                [python, "-m", "train.dataset.extract_f0", "cpu", str(experiment), str(workers), "rmvpe"],
                rvc,
            )
            run(
                "Reading timbre",
                0.55,
                [python, "-m", "train.dataset.extract_hubert_feature", "cpu", "1", "0", str(experiment), "v2", "False"],
                rvc,
            )

        # The file list training reads: every clip whose pitch and features
        # were both made, in RVC's own `wav|feature|f0|f0-without-semitone|speaker`
        # shape. Written here rather than by RVC, which only has a UI for it.
        gt = experiment / "0_gt_wavs"
        features = experiment / "3_feature768"
        f0 = experiment / "2a_f0"
        f0nsf = experiment / "2b-f0nsf"
        names = (
            {path.stem for path in gt.glob("*.wav")}
            & {path.stem for path in features.glob("*.npy")}
            & {path.name[: -len(".wav.npy")] for path in f0.glob("*.wav.npy")}
            & {path.name[: -len(".wav.npy")] for path in f0nsf.glob("*.wav.npy")}
        )
        if not names:
            return fail("no clip came through the split and feature steps: nothing to train on")
        lines = [
            "%s/%s.wav|%s/%s.npy|%s/%s.wav.npy|%s/%s.wav.npy|0"
            % (gt, clip, features, clip, f0, clip, f0nsf, clip)
            for clip in sorted(names)
        ]
        (experiment / "filelist.txt").write_text("\n".join(lines), encoding="utf8")
        emit(event="log", line="%d clips to train on, batch %d" % (len(lines), batch))

        run(
            "Training",
            0.7,
            [
                python,
                "-m",
                "train.train",
                "-e", name,
                "-sr", "48k",
                "-f0", "1",
                "-bs", str(batch),
                "-g", "0",
                "-te", str(epochs),
                "-se", str(save_every),
                "-pg", "assets/pretrained_v2/f0G48k.pth",
                "-pd", "assets/pretrained_v2/f0D48k.pth",
                "-l", "0",
                "-c", "0",
                "-sw", "1",
                "-v", "v2",
            ],
            rvc,
            {"RVC_CUDA_GRAPH": "0"},
        )

        run("Building the index", 0.94, [python, "-m", "train.train_index", name, "v2", str(experiment), str(workers), "single"], rvc)

        # Each saved checkpoint becomes a model this tool can swap with; the
        # index is one file shared by all of them.
        produced = sorted(
            (rvc / "assets" / "weights").glob("%s_e*_s*.pth" % name),
            key=lambda path: int(re.search(r"_e(\d+)_", path.name).group(1)) if re.search(r"_e(\d+)_", path.name) else 0,
        )
        final = rvc / "assets" / "weights" / ("%s.pth" % name)
        indexes = sorted(experiment.glob("added_*.index"), key=lambda path: path.stat().st_mtime)
        if not indexes:
            return fail("training finished without an index")
        if not produced and final.is_file():
            produced = [final]
        if not produced:
            return fail("training finished without producing a model")

        # Only now, with everything validated, is the previous model replaced:
        # a run that fails above must leave a working voice working.
        for stale in output.glob("model-e*.pth"):
            stale.unlink(missing_ok=True)
        if final.is_file():
            shutil.copyfile(final, output / ("model-e%d.pth" % epochs))
        for path in produced:
            match = re.search(r"_e(\d+)_", path.name)
            if not match:
                continue
            shutil.copyfile(path, output / ("model-e%s.pth" % match.group(1)))
        shutil.copyfile(indexes[-1], output / "model.index")

        shutil.rmtree(experiment, ignore_errors=True)
        for stale in (rvc / "assets" / "weights").glob("%s*" % name):
            stale.unlink(missing_ok=True)

        stage("Done", 1.0, "%d checkpoints" % len(produced))
        emit(event="done", output=str(output))
        return 0
    except (RuntimeError, OSError) as error:
        return fail(str(error))


def safe_cuda() -> bool:
    try:
        import torch

        return bool(torch.cuda.is_available())
    except Exception:  # noqa: BLE001 - no torch means no card
        return False


def convert(root: Path, job) -> int:
    rvc = root / "rvc"
    output = Path(job["output"])
    output.parent.mkdir(parents=True, exist_ok=True)

    arguments = [
        sys.executable,
        "-m",
        "infer.cli",
        "--model", str(job["model"]),
        "--input", str(job["source"]),
        "--output", str(output),
        "--pitch", str(job.get("pitch", 0)),
        "--f0-method", job.get("f0_method", "rmvpe"),
        "--index-rate", str(job.get("index_rate", 0.75)),
        "--protect", str(job.get("protect", 0.33)),
        "--rms-mix-rate", str(job.get("rms_mix_rate", 1.0)),
        "--overwrite",
    ]
    if job.get("index"):
        arguments += ["--index", str(job["index"])]
    if job.get("resample_sr"):
        arguments += ["--resample-sr", str(job["resample_sr"])]

    try:
        # Unbuffered: RVC prints the device, model and index before it starts,
        # and a pipe would hold them until the process ended.
        run("Converting", 0.1, arguments, rvc, {"PYTHONUNBUFFERED": "1"})
    except RuntimeError as error:
        return fail(str(error))
    if not output.is_file():
        return fail("RVC finished without writing %s" % output)
    stage("Done", 1.0)
    emit(event="done", output=str(output))
    return 0


def batch_for_card() -> int:
    """Half the card's memory, as upstream's UI does, clamped to a sane range."""
    try:
        import torch

        if not torch.cuda.is_available():
            return 1
        memory_gb = torch.cuda.get_device_properties(0).total_memory / (1024**3)
        return max(1, min(40, int(memory_gb // 2)))
    except Exception:  # noqa: BLE001 - no torch means no card
        return 1


def half_precision() -> bool:
    """The rule RVC's own config uses: fp16 only where the card is known good."""
    try:
        import torch

        if not torch.cuda.is_available():
            return False
        major, minor = torch.cuda.get_device_capability(0)
        name = torch.cuda.get_device_name(0)
        memory_gb = torch.cuda.get_device_properties(0).total_memory / (1024**3)
        if memory_gb < 3.6 or (major, minor) == (6, 1):
            return False
        if re.search(r"16\d{2}", name) and (major, minor) == (7, 5):
            return False
        return (major + minor / 10) > 6.1
    except Exception:  # noqa: BLE001 - no torch means no card
        return False


def main() -> int:
    parser = argparse.ArgumentParser(description="Voice swap: one job per process.")
    parser.add_argument("command", choices=["selftest", "train", "convert"])
    parser.add_argument("--root", required=True, help="the voice-swap folder holding rvc/ and venv/")
    parser.add_argument("--job", help="a JSON file describing the job")
    options = parser.parse_args()

    root = Path(options.root).resolve()
    if options.command == "selftest":
        return selftest(root)
    if not options.job:
        return fail("%s needs --job" % options.command)
    with open(options.job, encoding="utf8") as handle:
        job = json.load(handle)
    return train(root, job) if options.command == "train" else convert(root, job)


if __name__ == "__main__":
    sys.exit(main())
