# /// script
# requires-python = ">=3.10"
# dependencies = ["pyyaml==6.0.3"]
# ///
"""Fetch the third-party files the build needs into git-ignored folders of src-tauri/.

This is the one place that pins them, each by URL and SHA-256:

- src-tauri/pdfium/: the pdfium library the app bundles (bblanchon/pdfium-binaries), per
  platform. Update it in lockstep with iscc-sdk, because pdfium's own updates change extracted
  text. The library and the licence files are extracted flat, because Tauri's resource glob
  takes only the files directly in the directory.
- src-tauri/ocr/: the PP-OCRv6 tiny text detection and recognition models (PaddlePaddle,
  Apache-2.0) that ocr.rs compiles into the binaries, the same on every platform, and the
  recognition dictionary, one entry per line, written from the model's inference.yml so the app
  parses no YAML.

Nothing is downloaded when the right files are already there.

    uv run scripts/fetch_resources.py [--target win-x64|linux-x64|mac-univ|mac-arm64]
"""

import argparse
import hashlib
import io
import platform
import shutil
import sys
import tarfile
import urllib.request
from pathlib import Path

import yaml

SRC_TAURI = Path(__file__).resolve().parent.parent / "src-tauri"
STAMP = "BUILD.txt"

PDFIUM_BUILD = "chromium/8076"

PDFIUM_SHA256 = {
    "win-x64": "808d36da9bc5a3104315fb307c80998121f565ee53953633bf33e80d7429e5ac",
    "linux-x64": "d9d67bc40af03aef4fe28a60b19b1086f28ace019c8c9caf19cb7fe3d14ceca3",
    "mac-univ": "3bdb93e229298dfdf083dc8ccc7d1a8cf87790b6917e5073335504fe2ff0bdc1",
    "mac-arm64": "0d6781fe08906baff3d82c90953e519fbc4eb253fe76431e5ed53b157763b97c",
}

PDFIUM_LIBRARY = {
    "win-x64": "bin/pdfium.dll",
    "linux-x64": "lib/libpdfium.so",
    "mac-univ": "lib/libpdfium.dylib",
    "mac-arm64": "lib/libpdfium.dylib",
}

PDFIUM_DEST = SRC_TAURI / "pdfium"

HUGGING_FACE = "https://huggingface.co/PaddlePaddle"
OCR_DET = f"{HUGGING_FACE}/PP-OCRv6_tiny_det_onnx/resolve/2ba1506c0380b8f0b03dd142459aac66d4421f6c"
OCR_REC = f"{HUGGING_FACE}/PP-OCRv6_tiny_rec_onnx/resolve/2612ab37152ae0a677521bae4e1e3d4fb4cf7c30"

# File in OCR_DEST: (URL, SHA-256 of the download).
OCR_FILES = {
    "PP-OCRv6_tiny_det.onnx": (
        f"{OCR_DET}/inference.onnx",
        "193bab7a04fca699a6c82e6abb5b81bdb28177f0abd4062552b04908dafb19f8",
    ),
    "PP-OCRv6_tiny_rec.onnx": (
        f"{OCR_REC}/inference.onnx",
        "9ef676d6ed3c88256a2d92c640c44f25b0c40947e111b14b8be8f594091563e6",
    ),
}
OCR_REC_YML = (
    f"{OCR_REC}/inference.yml",
    "66170210bad538e83fff3c4a3867e547d6bf20b50d64b20347c4b913f3034ea1",
)
OCR_DICT = "PP-OCRv6_tiny_rec.txt"
OCR_DICT_SHA256 = "c5cbe34ef40c29c4df07ed012bf96569cb69a2d2a01a07027e9f13cb832bd9cd"

OCR_DEST = SRC_TAURI / "ocr"


def host_target() -> str:
    """pdfium archive name of the machine this runs on."""
    system = platform.system()
    if system == "Windows":
        return "win-x64"
    if system == "Darwin":
        return "mac-univ"
    if system == "Linux":
        return "linux-x64"
    sys.exit(f"no pdfium archive for {system}; pass --target")


def download(url: str, sha256: str) -> bytes:
    """The file at url, checked against its pinned SHA-256."""
    print(f"downloading {url}", flush=True)
    with urllib.request.urlopen(url, timeout=120) as response:
        data = response.read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != sha256:
        sys.exit(f"SHA-256 mismatch for {url}: {digest}")
    return data


def is_current(dest: Path, stamp: str, files: list[str]) -> bool:
    """Whether dest holds every file of files and a stamp file reading stamp."""
    marker = dest / STAMP
    present = all((dest / name).is_file() for name in files)
    return present and marker.is_file() and marker.read_text() == stamp


def pdfium_stamp(target: str) -> str:
    """Contents of the stamp file that marks a complete pdfium extraction."""
    return f"{PDFIUM_BUILD} {target} sha256:{PDFIUM_SHA256[target]}\n"


def flat_name(member: str, target: str) -> str | None:
    """File name in PDFIUM_DEST for an archive member, or None to skip it."""
    if member == PDFIUM_LIBRARY[target]:
        return Path(member).name
    if member == "LICENSE":
        return "LICENSE-pdfium-binaries.txt"
    if member.startswith("licenses/"):
        return f"LICENSE-{Path(member).name}"
    return None


def fetch_pdfium(target: str) -> None:
    """Replace PDFIUM_DEST with the library and licence files of the pinned build for target."""
    library = Path(PDFIUM_LIBRARY[target]).name
    if is_current(PDFIUM_DEST, pdfium_stamp(target), [library]):
        print(f"pdfium {PDFIUM_BUILD} {target} is present", flush=True)
        return
    tag = PDFIUM_BUILD.replace("/", "%2F")
    url = f"https://github.com/bblanchon/pdfium-binaries/releases/download/{tag}/pdfium-{target}.tgz"
    data = download(url, PDFIUM_SHA256[target])
    shutil.rmtree(PDFIUM_DEST, ignore_errors=True)
    PDFIUM_DEST.mkdir(parents=True)
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
        for member in tar.getmembers():
            name = flat_name(member.name, target)
            if member.isfile() and name:
                (PDFIUM_DEST / name).write_bytes(tar.extractfile(member).read())
    (PDFIUM_DEST / STAMP).write_text(pdfium_stamp(target))
    print(f"pdfium {PDFIUM_BUILD} {target} extracted to {PDFIUM_DEST}", flush=True)


def ocr_stamp() -> str:
    """Contents of the stamp file that marks complete OCR files."""
    lines = [f"{name} sha256:{sha}" for name, (_, sha) in OCR_FILES.items()]
    return "\n".join([*lines, f"{OCR_DICT} sha256:{OCR_DICT_SHA256}"]) + "\n"


def dictionary(yml: bytes) -> bytes:
    """The recognition dictionary of the model's inference.yml, one entry per line, checked."""
    entries = yaml.safe_load(yml)["PostProcess"]["character_dict"]
    if not all(isinstance(e, str) and len(e) == 1 and e != "\n" for e in entries):
        sys.exit("the recognition dictionary has an entry that is not one character")
    text = ("\n".join(entries) + "\n").encode("utf-8")
    digest = hashlib.sha256(text).hexdigest()
    if digest != OCR_DICT_SHA256:
        sys.exit(f"SHA-256 mismatch for the recognition dictionary: {digest}")
    return text


def fetch_ocr() -> None:
    """Replace OCR_DEST with the pinned OCR models and the recognition dictionary."""
    if is_current(OCR_DEST, ocr_stamp(), [*OCR_FILES, OCR_DICT]):
        print("PP-OCRv6 tiny models are present", flush=True)
        return
    files = {name: download(url, sha) for name, (url, sha) in OCR_FILES.items()}
    files[OCR_DICT] = dictionary(download(*OCR_REC_YML))
    shutil.rmtree(OCR_DEST, ignore_errors=True)
    OCR_DEST.mkdir(parents=True)
    for name, data in files.items():
        (OCR_DEST / name).write_bytes(data)
    (OCR_DEST / STAMP).write_text(ocr_stamp())
    print(f"PP-OCRv6 tiny models written to {OCR_DEST}", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--target", choices=sorted(PDFIUM_SHA256), default=None)
    fetch_pdfium(parser.parse_args().target or host_target())
    fetch_ocr()


if __name__ == "__main__":
    main()
