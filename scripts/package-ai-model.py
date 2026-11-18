#!/usr/bin/env python3
"""Validate the installed model and create an offline ZIP accepted by PhotoSys."""
import argparse
import hashlib
from pathlib import Path
import zipfile

FILES = {
    "cn_clip_text.onnx": (409585120, "5ddc2d8971b09acda8063048003d18cca9b587f089a1c0a4df7d35624bd0fcea"),
    "cn_clip_vision.onnx": (345839119, "980020bf226e528a202de26c6a6186df125b74c612b09cfcd797a082e046b628"),
    "vocab.txt": (109540, "45bbac6b341c319adc98a532532882e91a9cefc0329aa57bac9ae761c27b291c"),
}

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("directory", type=Path, help="directory containing the three model files")
parser.add_argument("output", type=Path, help="output ZIP path")
args = parser.parse_args()
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
    archive.write(Path(__file__).parent / "AI-MODEL-NOTICES.md", "README.txt")
print(args.output.resolve())
