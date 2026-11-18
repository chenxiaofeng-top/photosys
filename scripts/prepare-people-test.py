#!/usr/bin/env python3
"""Fetch pinned public face models and an OpenCV sample for explicit native tests.
Nothing is downloaded by ordinary cargo tests or by face analysis in the app.
"""
import argparse
import hashlib
from pathlib import Path
import subprocess
import sys
import urllib.request

FILES = [
    ("face_detection_yunet_2023mar.onnx", 232589, "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4", "https://huggingface.co/opencv/face_detection_yunet/resolve/3cc26e7f1014a5ee5d74a42acee58bafc9d0a310/face_detection_yunet_2023mar.onnx"),
    ("face_recognition_sface_2021dec.onnx", 38696353, "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79", "https://huggingface.co/opencv/face_recognition_sface/resolve/3d7082438a6e4551e840c9b2bb60b71e8da4b524/face_recognition_sface_2021dec.onnx"),
    ("lena.jpg", 91814, "7de7ed51a1594fff247f4cae2301eceacf5313d6011e37b4a4c8733f7bb72c07", "https://raw.githubusercontent.com/opencv/opencv/4.12.0/samples/data/lena.jpg"),
]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("directory", type=Path)
args = parser.parse_args()
args.directory.mkdir(parents=True, exist_ok=True)
for name, size, digest, url in FILES:
    dest = args.directory / name
    if dest.is_symlink():
        parser.error(f"Refusing symlink: {dest}")
    if dest.is_file() and dest.stat().st_size == size and hashlib.sha256(dest.read_bytes()).hexdigest() == digest:
        continue
    partial = dest.with_suffix(dest.suffix + ".download")
    if partial.is_symlink():
        parser.error(f"Refusing symlink: {partial}")
    with urllib.request.urlopen(url, timeout=90) as response, partial.open("wb") as stream:
        received = 0
        while data := response.read(128 * 1024):
            received += len(data)
            if received > size:
                raise ValueError(f"{name}: response exceeds pinned size")
            stream.write(data)
    if received != size or hashlib.sha256(partial.read_bytes()).hexdigest() != digest:
        raise ValueError(f"{name}: pinned size or SHA-256 mismatch")
    partial.replace(dest)
    print(f"Verified {name}")
package = args.directory / "photosys-people.zip"
subprocess.run([sys.executable, str(Path(__file__).with_name("package-ai-model.py")), "--kind", "people", str(args.directory), str(package)], check=True)
print(f"PHOTOSYS_PEOPLE_MODEL_DIR={args.directory.resolve()}")
print(f"PHOTOSYS_PEOPLE_PACKAGE={package.resolve()}")
