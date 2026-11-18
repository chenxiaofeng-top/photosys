#!/usr/bin/env python3
"""Validate the installed model and create an offline ZIP accepted by PhotoSys."""
import argparse
import hashlib
from pathlib import Path
import zipfile

SEARCH_FILES = {
    "cn_clip_text.onnx": (409585120, "5ddc2d8971b09acda8063048003d18cca9b587f089a1c0a4df7d35624bd0fcea"),
    "cn_clip_vision.onnx": (345839119, "980020bf226e528a202de26c6a6186df125b74c612b09cfcd797a082e046b628"),
    "vocab.txt": (109540, "45bbac6b341c319adc98a532532882e91a9cefc0329aa57bac9ae761c27b291c"),
}

PEOPLE_FILES = {
    "face_detection_yunet_2023mar.onnx": (232589, "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4"),
    "face_recognition_sface_2021dec.onnx": (38696353, "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79"),
}

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("directory", type=Path, help="directory containing the selected model files")
parser.add_argument("output", type=Path, help="output ZIP path")
parser.add_argument("--kind", choices=["search", "people"], default="search")
args = parser.parse_args()
FILES = PEOPLE_FILES if args.kind == "people" else SEARCH_FILES
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
    archive.write(Path(__file__).parent / ("PEOPLE-MODEL-NOTICES.md" if args.kind == "people" else "AI-MODEL-NOTICES.md"), "README.txt")
print(args.output.resolve())
