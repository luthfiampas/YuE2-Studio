#!/usr/bin/env python3
"""De-reverb runner.

One process per run, like the voice swap runner: JSON lines on stdout, and it
exits when the work is done. The model is UVR's `UVR-DeEcho-DeReverb`, loaded
through `audio-separator` in the shared Python environment.

    {"event":"stage","stage":"Removing reverb","progress":0.3}
    {"event":"done"}
"""

import argparse
import json
import sys
from pathlib import Path


def emit(**event):
    sys.stdout.write(json.dumps(event, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def stage(name, progress=None):
    emit(event="stage", stage=name, progress=progress)


def fail(message):
    emit(event="error", message=message)
    return 1


def run(root: Path, job) -> int:
    source = Path(job["source"])
    output = Path(job["output"])
    model = Path(job["model"])
    output.mkdir(parents=True, exist_ok=True)

    stage("Loading the model", 0.1)
    try:
        from audio_separator.separator import Separator
    except Exception as error:  # noqa: BLE001 - the message is the report
        return fail("audio-separator does not load: %s" % error)

    try:
        separator = Separator(
            model_file_dir=str(model.parent),
            output_dir=str(output),
            output_format="wav",
            output_single_stem=None,
        )
        separator.load_model(model_filename=model.name)
    except Exception as error:  # noqa: BLE001
        return fail("the model did not load: %s" % error)

    stage("Removing reverb", 0.3)
    try:
        produced = separator.separate(str(source))
    except Exception as error:  # noqa: BLE001
        return fail("the separation failed: %s" % error)

    stage("Writing the files", 0.9)
    # audio-separator names its stems after the model: "(No Reverb)" is the dry
    # take, "(Reverb)" what it took out.
    def resolved(path):
        candidate = Path(path)
        return candidate if candidate.is_absolute() else output / candidate

    files = [resolved(path) for path in produced]
    # This model names its stems "(No Reverb)" and "(Reverb)"; the dry one is
    # whichever says "no", and the other is what was taken out.
    dry = next((path for path in files if "no reverb" in path.name.lower()), files[0] if files else None)
    wet = next((path for path in files if path != dry), None)
    if not dry:
        return fail("the model wrote no dry stem")
    if not wet:
        return fail("the model wrote no reverb stem")

    # Renamed so nothing downstream depends on the model's wording.
    Path(output, "dry.wav").unlink(missing_ok=True)
    Path(dry).replace(output / "dry.wav")
    if wet:
        Path(output, "reverb.wav").unlink(missing_ok=True)
        Path(wet).replace(output / "reverb.wav")
        emit(event="log", line="reverb stem written")

    stage("Done", 1.0)
    emit(event="done", output=str(output))
    return 0


def selftest(root: Path) -> int:
    stage("Python", 0.2)
    try:
        import torch  # noqa: F401
    except Exception as error:  # noqa: BLE001
        return fail("torch does not load in this environment: %s" % error)
    try:
        import audio_separator  # noqa: F401
    except Exception as error:  # noqa: BLE001
        return fail("audio-separator does not load: %s" % error)
    stage("Model", 0.6)
    model = root / "models" / "UVR-DeEcho-DeReverb.pth"
    if not model.is_file():
        return fail("the de-reverb model is not installed")
    stage("Ready", 1.0)
    emit(event="done")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="De-reverb: one run per process.")
    parser.add_argument("command", choices=["selftest", "run"])
    parser.add_argument("--root", required=True)
    parser.add_argument("--job")
    options = parser.parse_args()

    root = Path(options.root).resolve()
    if options.command == "selftest":
        return selftest(root)
    if not options.job:
        return fail("run needs --job")
    with open(options.job, encoding="utf8") as handle:
        job = json.load(handle)
    return run(root, job)


if __name__ == "__main__":
    sys.exit(main())
