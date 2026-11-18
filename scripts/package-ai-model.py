#!/usr/bin/env python3
"""Validate the installed model and create an offline ZIP accepted by PhotoSys."""
import argparse
import hashlib
from pathlib import Path
import zipfile

import json

CATALOG = json.loads((Path(__file__).parent / "ai-model-catalog.json").read_text())
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("directory", type=Path, help="directory containing the selected model files")
parser.add_argument("output", type=Path, help="output ZIP path")
parser.add_argument("--kind", choices=["search", "people"], default="search")
parser.add_argument("--model-id", help="exact model ID from ai-model-catalog.json; defaults to the legacy model for --kind")
args = parser.parse_args()
model = next((m for m in CATALOG if m["id"] == args.model_id), None) if args.model_id else next(m for m in CATALOG if m["kind"] == args.kind)
if model is None:
    parser.error("Unknown model ID")
# An explicit ID identifies its kind, so --kind is optional with --model-id.
FILES = {a["name"]: (a["size"], a["sha"]) for a in model["assets"]}
if args.output.resolve() in [(args.directory / name).resolve() for name in FILES]:
    parser.error("Output must not overwrite a model file")
for name, (size, expected) in FILES.items():
    path = args.directory / name
    if path.is_symlink() or not path.is_file() or path.stat().st_size != size:
        parser.error(f"Invalid model file: {name}")
    with path.open("rb") as stream:
        if hashlib.file_digest(stream, "sha256").hexdigest() != expected:
            parser.error(f"SHA-256 mismatch: {name}")
with zipfile.ZipFile(args.output, "w", compression=zipfile.ZIP_STORED) as archive:
    for name in FILES:
        archive.write(args.directory / name, name)
    archive.writestr("photosys-model.json", json.dumps({"schema": 1, "modelId": model["id"]}))
    archive.write(Path(__file__).parent / ("PEOPLE-MODEL-NOTICES.md" if model["kind"] == "people" else "AI-MODEL-NOTICES.md"), "README.txt")
print(args.output.resolve())
