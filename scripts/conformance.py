# /// script
# requires-python = ">=3.10"
# dependencies = [
#   "pyyaml==6.0.3",
#   "json-formula @ git+https://github.com/adobe/json-formula-py@327a8d75669673c10db3051da816311ac2df8a32",
# ]
# ///
"""Check the manifests the app signs against the C2PA conformance rubrics.

Builds the CLI, signs one fixture per kind of container and hard binding with it, writes
c2pa-rs's crJSON of each signed copy (the CLI's hidden `crjson` command) and evaluates it with
the C2PA Conformance Task Force's asset rubric and evaluator. Both come from
c2pa-org/conformance-public at a pinned commit, each file checked by SHA-256, into
build/conformance/. The signed copies and their crJSON stay in build/conformance/signed/.

The rubric is the newest one for the Conformance Program, evaluated for the specification
version the app declares (context::C2PA_SPEC_VERSION). The rubrics for spec 2.2 are not run:
their spec-version rule rejects a manifest that declares a later version. Every trait must hold
on every file, except the known failures below; a known failure that holds everywhere fails the
check too, so the list does not outlive its reason.

With C2PA_CONFORMANCE_DIR set to a checkout of a newer rubric release (a folder with
asset-rubrics/), its rubric and evaluator also run on the same files, as a preview: only the
soft-binding traits must hold there, the other traits that fail are listed.

    uv run scripts/conformance.py
"""

import hashlib
import json
import os
import shutil
import subprocess
import sys
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SRC_TAURI = REPO / "src-tauri"
FIXTURES = SRC_TAURI / "tests" / "fixtures"
WORK = REPO / "build" / "conformance"

# conformance-public at the sync of asset rubric release 1.0 (2026-08-06).
COMMIT = "7dbff513b66e36184619286fe734beea32f2e6db"
BASE_URL = f"https://raw.githubusercontent.com/c2pa-org/conformance-public/{COMMIT}/asset-rubrics"
EVALUATOR = "c2pa_conformance_rubric_evaluator.py"
RUBRIC = "asset-rubric-conformance0.2-spec2.4.yml"
FILES = {
    EVALUATOR: "537c45e88a603aeb08f8777cdc160bffc9fa6a3ef2a7f3f68c46137fbd7ee2d4",
    RUBRIC: "63e9dd777fbb9d1295efe086864c88475f8cae8d944ecb7b1ff21b718c4bade9",
}

# The specification version the app declares, which the rubric's spec-version rule asks for.
SPEC_VERSION = "2.4"

# Fixture: extra arguments of `sign`. One per kind of container and hard binding.
SIGNED = {
    # JPEG without credentials, with a digital source type and a training and data mining entry.
    "no_manifest.jpg": [
        "--source-type",
        "http://cv.iptc.org/newscodes/digitalsourcetype/digitalCapture",
        "--training",
        "cawg.ai_training=notAllowed",
    ],
    "CA.jpg": [],  # JPEG with Content Credentials: a parent ingredient with a manifest
    "meta-xmp.png": [],
    "demo.tif": [],
    "demo.svg": [],
    "demo.mp3": [],
    "demo.wav": [],
    "demo.mp4": [],  # BMFF hash; read without its Content-Code when ffmpeg is not installed
    "demo.pdf": [],
    "demo.docx": [],  # collection of zip entries
    "demo.txt": [],
    "demo.md": [],
}

# Trait: why it fails for now. Empty with rubric release 1.0.
KNOWN_FAILURES: dict[str, str] = {}

SOFT_BINDING = "soft_binding"


def sha256(path: Path) -> str:
    """SHA-256 of the file at path, in hex."""
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fetch_rubrics() -> Path:
    """Folder holding the pinned evaluator and rubric, each downloaded unless present and intact."""
    dest = WORK / COMMIT[:12]
    dest.mkdir(parents=True, exist_ok=True)
    for name, digest in FILES.items():
        path = dest / name
        if path.is_file() and sha256(path) == digest:
            continue
        print(f"downloading {BASE_URL}/{name}", flush=True)
        with urllib.request.urlopen(f"{BASE_URL}/{name}", timeout=60) as response:
            data = response.read()
        if hashlib.sha256(data).hexdigest() != digest:
            sys.exit(f"SHA-256 mismatch for {name}: {hashlib.sha256(data).hexdigest()}")
        path.write_bytes(data)
    return dest


def build_cli() -> Path:
    """Path of the CLI, built in the dev profile as the tests build it."""
    manifest = SRC_TAURI / "Cargo.toml"
    subprocess.run(
        ["cargo", "build", "--manifest-path", manifest, "--features", "cli", "--bin", "c2pa-iscc"],
        check=True,
    )
    suffix = ".exe" if sys.platform == "win32" else ""
    return SRC_TAURI / "target" / "debug" / f"c2pa-iscc{suffix}"


def run(command: list) -> bytes:
    """Standard output of command; exits with its standard error when it fails."""
    result = subprocess.run(command, capture_output=True)
    if result.returncode:
        stderr = result.stderr.decode("utf-8", "replace")
        sys.exit(f"{' '.join(map(str, command))} failed:\n{stderr}")
    return result.stdout


def sign_and_export(cli: Path, fixture: str, extra: list[str], out: Path) -> Path:
    """crJSON file of a signed copy of fixture, signed without a timestamp."""
    signed = out / f"signed-{fixture}"
    options = ["--output", signed, "--no-timestamp", "--no-preview", "--compact", *extra]
    run([cli, "sign", FIXTURES / fixture, *options])
    crjson = out / f"{fixture}.crjson.json"
    crjson.write_bytes(run([cli, "crjson", signed]))
    return crjson


def evaluate(rubrics: Path, crjson: Path) -> dict[str, list[dict]]:
    """Traits of the rubric in rubrics for crjson, by outcome ("true", "false", "warning").

    The evaluator runs isolated (-I: nothing imported from its folder or the working directory)
    and in UTF-8 mode, so it reads the crJSON and prints its report in UTF-8 on every platform.
    """
    evaluator = [sys.executable, "-I", "-X", "utf8", rubrics / EVALUATOR, rubrics / RUBRIC]
    output = run([*evaluator, crjson, "--spec-version", SPEC_VERSION])
    report = json.loads(output.decode("utf-8"))
    outcomes = ("true", "false", "warning")
    return {o: [t for traits in report[o].values() for t in traits] for o in outcomes}


def failing(report: dict[str, list[dict]]) -> list[dict]:
    """Traits of a report that do not hold: false or a warning."""
    return report["false"] + report["warning"]


def gate_problems(reports: dict[str, dict]) -> list[str]:
    """What fails the check: a trait that does not hold and is no known failure, a known failure
    that is a soft-binding trait, and a known failure that holds on every file."""
    problems = []
    for fixture, report in reports.items():
        for trait in failing(report):
            name = trait["trait"]
            if name not in KNOWN_FAILURES or SOFT_BINDING in name:
                problems.append(f"{fixture}: {name}: {trait['reportText']}")
    seen = {t["trait"] for report in reports.values() for t in failing(report)}
    stale = [name for name in KNOWN_FAILURES if name not in seen]
    problems += [f"known failure {name} holds on every file; remove it" for name in stale]
    return problems


def preview_problems(reports: dict[str, dict]) -> list[str]:
    """What fails the preview: a soft-binding trait that does not hold. Prints the other ones."""
    problems = []
    for fixture, report in reports.items():
        for trait in failing(report):
            line = f"{fixture}: {trait['trait']}: {trait['reportText']}"
            if SOFT_BINDING in trait["trait"]:
                problems.append(line)
            else:
                print(f"  note: {line}", flush=True)
    return problems


def summary(reports: dict[str, dict]) -> None:
    """Print how many traits hold per fixture, and how many are about the soft binding."""
    for fixture, report in reports.items():
        held = len(report["true"])
        total = held + len(failing(report))
        soft = sum(SOFT_BINDING in t["trait"] for t in report["true"] + failing(report))
        print(f"  {fixture}: {held} of {total} traits hold ({soft} on soft bindings)", flush=True)


def main() -> None:
    rubrics = fetch_rubrics()
    cli = build_cli()
    out = WORK / "signed"
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    crjsons = {name: sign_and_export(cli, name, extra, out) for name, extra in SIGNED.items()}

    print(f"{RUBRIC} (conformance-public {COMMIT[:7]}), spec {SPEC_VERSION}:", flush=True)
    reports = {name: evaluate(rubrics, crjson) for name, crjson in crjsons.items()}
    summary(reports)
    problems = gate_problems(reports)

    newer = os.environ.get("C2PA_CONFORMANCE_DIR")
    if newer:
        preview = Path(newer) / "asset-rubrics"
        print(f"preview with {preview / RUBRIC}:", flush=True)
        previews = {name: evaluate(preview, crjson) for name, crjson in crjsons.items()}
        summary(previews)
        problems += [f"preview: {p}" for p in preview_problems(previews)]

    for problem in problems:
        print(f"FAIL {problem}", flush=True)
    if problems:
        sys.exit(1)
    print("conformance check passed", flush=True)


if __name__ == "__main__":
    main()
