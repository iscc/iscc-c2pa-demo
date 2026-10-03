# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Fetch the pdfium library the app bundles (bblanchon/pdfium-binaries) into src-tauri/pdfium/.

The build is pinned here and only here. Update it in lockstep with iscc-sdk, because pdfium's
own updates change extracted text. The archive is checked against its pinned SHA-256; the
library and the licence files are extracted flat, because Tauri's resource glob takes only the
files directly in the directory. Nothing happens when the right build is already there.

    uv run scripts/fetch_pdfium.py [--target win-x64|linux-x64|mac-univ|mac-arm64]
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

PDFIUM_BUILD = "chromium/8076"

SHA256 = {
    "win-x64": "808d36da9bc5a3104315fb307c80998121f565ee53953633bf33e80d7429e5ac",
    "linux-x64": "d9d67bc40af03aef4fe28a60b19b1086f28ace019c8c9caf19cb7fe3d14ceca3",
    "mac-univ": "3bdb93e229298dfdf083dc8ccc7d1a8cf87790b6917e5073335504fe2ff0bdc1",
    "mac-arm64": "0d6781fe08906baff3d82c90953e519fbc4eb253fe76431e5ed53b157763b97c",
}

LIBRARY = {
    "win-x64": "bin/pdfium.dll",
    "linux-x64": "lib/libpdfium.so",
    "mac-univ": "lib/libpdfium.dylib",
    "mac-arm64": "lib/libpdfium.dylib",
}

DEST = Path(__file__).resolve().parent.parent / "src-tauri" / "pdfium"
STAMP = "BUILD.txt"


def host_target() -> str:
    """Archive name of the machine this runs on."""
    system = platform.system()
    if system == "Windows":
        return "win-x64"
    if system == "Darwin":
        return "mac-univ"
    if system == "Linux":
        return "linux-x64"
    sys.exit(f"no pdfium archive for {system}; pass --target")


def stamp_text(target: str) -> str:
    """Contents of the stamp file that marks a complete extraction."""
    return f"{PDFIUM_BUILD} {target} sha256:{SHA256[target]}\n"


def is_current(target: str) -> bool:
    """Whether DEST already holds the pinned build for target."""
    stamp = DEST / STAMP
    library = DEST / Path(LIBRARY[target]).name
    return library.is_file() and stamp.is_file() and stamp.read_text() == stamp_text(target)


def download(target: str) -> bytes:
    """The archive for target, checked against its pinned SHA-256."""
    tag = PDFIUM_BUILD.replace("/", "%2F")
    url = f"https://github.com/bblanchon/pdfium-binaries/releases/download/{tag}/pdfium-{target}.tgz"
    print(f"downloading {url}", flush=True)
    with urllib.request.urlopen(url, timeout=120) as response:
        data = response.read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != SHA256[target]:
        sys.exit(f"SHA-256 mismatch for pdfium-{target}.tgz: {digest}")
    return data


def flat_name(member: str, target: str) -> str | None:
    """File name in DEST for an archive member, or None to skip it."""
    if member == LIBRARY[target]:
        return Path(member).name
    if member == "LICENSE":
        return "LICENSE-pdfium-binaries.txt"
    if member.startswith("licenses/"):
        return f"LICENSE-{Path(member).name}"
    return None


def extract(data: bytes, target: str) -> None:
    """Replace DEST with the library and licence files of the archive."""
    shutil.rmtree(DEST, ignore_errors=True)
    DEST.mkdir(parents=True)
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
        for member in tar.getmembers():
            name = flat_name(member.name, target)
            if member.isfile() and name:
                (DEST / name).write_bytes(tar.extractfile(member).read())
    (DEST / STAMP).write_text(stamp_text(target))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--target", choices=sorted(SHA256), default=None)
    target = parser.parse_args().target or host_target()
    if is_current(target):
        print(f"pdfium {PDFIUM_BUILD} {target} is present", flush=True)
        return
    extract(download(target), target)
    print(f"pdfium {PDFIUM_BUILD} {target} extracted to {DEST}", flush=True)


if __name__ == "__main__":
    main()
