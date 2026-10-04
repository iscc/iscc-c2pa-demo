"""Generate reference values for the video fixtures with iscc-sdk (ffmpeg and ffprobe 8.1).

Run: uv run --with iscc-sdk expected_video.py

For every video file in this folder it records the Content-Code Video (MPEG-7 frame signatures
from ffmpeg's signature filter through `gen_video_code_v0`, null when ffmpeg yields no frames),
the name, description, ISCC metadata and creator `video_meta_extract` reads, the 256-bit
Meta-Code (embedded name, else the file name, as `code_meta` does), the Data-Code and the
Instance-Code. It checks that ffprobe adds nothing to the Meta-Code inputs that ffmpeg's
ffmetadata output alone does not give, which is why the app downloads ffmpeg only.

It also writes demo.mp4.mp7sig, the raw signature of demo.mp4, and records its frame count and
first frame vector, so the signature parser is tested without ffmpeg.
"""

import json
import subprocess
from pathlib import Path

import iscc_lib as il
import iscc_sdk as idk

from expected_text import meta_code

HERE = Path(__file__).parent
BITS = 256
VIDEO_SUFFIXES = {".mp4", ".mov", ".m4v", ".avi"}
FIELDS = ("name", "description", "meta", "creator")


def merged_meta(path):
    """Metadata as `video_meta_extract` merges ffprobe's and ffmpeg's, or ffmpeg's alone when
    ffprobe finds no video stream (iscc-sdk raises there)."""
    alone = idk.video.video_meta_extract_ffmpeg(path)
    try:
        merged = idk.video_meta_extract(path)
    except ValueError:
        return alone
    for field in FIELDS:
        # An empty ffprobe title stays in the merged result; both mean "no name".
        assert (merged.get(field) or None) == (alone.get(field) or None), (path.name, field)
    return merged


def video_code(path):
    """Content-Code Video like `iscc_sdk.code_video`, or None when there are no frames."""
    try:
        frames = idk.video_features_extract(path)
    except (subprocess.CalledProcessError, OSError):
        return None
    if not frames:
        return None
    return il.gen_video_code_v0(frames, bits=BITS)["iscc"]


def signature_reference():
    """Write demo.mp4's raw signature and return its frame count and first frame vector."""
    sig = idk.video_mp7sig_extract(HERE / "demo.mp4")
    (HERE / "demo.mp4.mp7sig").write_bytes(sig)
    frames = idk.read_mp7_signature(sig)
    return {
        "frames": len(frames),
        "first_frame": "".join(str(v) for v in frames[0].vector.tolist()),
        "first_confidence": frames[0].confidence,
    }


def main():
    out = {}
    for path in sorted(p for p in HERE.iterdir() if p.suffix.lower() in VIDEO_SUFFIXES):
        data = path.read_bytes()
        meta = merged_meta(path)
        out[path.name] = {
            "name": meta.get("name") or None,
            "description": meta.get("description"),
            "meta_field": meta.get("meta"),
            "creator": meta.get("creator"),
            "meta": meta_code(path, meta),
            "video": video_code(path),
            "data": il.gen_data_code_v0(data, bits=BITS)["iscc"],
            "instance": il.gen_instance_code_v0(data, bits=BITS)["iscc"],
        }
    out["demo.mp4"]["signature"] = signature_reference()
    (HERE / "expected_video.json").write_text(json.dumps(out, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps(out, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
