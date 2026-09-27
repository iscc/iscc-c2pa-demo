"""Generate reference values for the audio fixtures with iscc-sdk (fpcalc and TagLib).

Run: uv run --with iscc-sdk expected_audio.py

For every audio file in this folder it records the Content-Code Audio (fpcalc's Chromaprint
fingerprint through `gen_audio_code_v0`, null when fpcalc finds the audio too short), the name,
description, ISCC metadata, creator and duration `audio_meta_extract` reads with TagLib, the
256-bit Meta-Code (embedded name, else the file name, as `code_meta` does), the Data-Code and
the Instance-Code.
"""

import json
import subprocess
from pathlib import Path

import iscc_lib as il
import iscc_sdk as idk

from expected_text import meta_code

HERE = Path(__file__).parent
BITS = 256
AUDIO_SUFFIXES = {".mp3", ".flac", ".wav", ".m4a"}


def audio_code(path):
    """Content-Code Audio like `iscc_sdk.code_audio`, or None when fpcalc fails."""
    try:
        features = idk.audio_features_extract(path)
    except subprocess.CalledProcessError:
        return None
    return il.gen_audio_code_v0(features["fingerprint"], bits=BITS)["iscc"]


def main():
    out = {}
    for path in sorted(p for p in HERE.iterdir() if p.suffix.lower() in AUDIO_SUFFIXES):
        data = path.read_bytes()
        meta = idk.audio_meta_extract(path)
        out[path.name] = {
            "name": meta.get("name"),
            "description": meta.get("description"),
            "meta_field": meta.get("meta"),
            "creator": meta.get("creator"),
            "duration": meta.get("duration"),
            "meta": meta_code(path, meta),
            "audio": audio_code(path),
            "data": il.gen_data_code_v0(data, bits=BITS)["iscc"],
            "instance": il.gen_instance_code_v0(data, bits=BITS)["iscc"],
        }
    (HERE / "expected_audio.json").write_text(json.dumps(out, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps(out, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
