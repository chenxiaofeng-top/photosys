#!/usr/bin/env python3
"""Fetch pinned official CPU runtimes; no Python is needed by the shipped app."""
import argparse
import hashlib
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
# ort uses C API 22. Intel macOS's last upstream native archive is 1.22.0.
PACKAGES = {
    ("Linux", "x86_64"): ("1.24.2", "linux-x64", "43725474ba5663642e17684717946693850e2005efbd724ac72da278fead25e6"),
    ("Windows", "x86_64"): ("1.24.2", "win-x64", "8e3e9c826375352e29cb2614fe44f3d7a4b0ff7b8028ad7a456af9d949a7e8b0"),
    ("Darwin", "aarch64"): ("1.24.2", "osx-arm64", "0af4fa503e8ea285245b47ee42d0a7461b8156a81270857da0c1d4ecf858abde"),
    ("Darwin", "x86_64"): ("1.22.0", "osx-x86_64", "e4ec94a7696de74fb1b12846569aa94e499958af6ffa186022cfde16c9d617f0"),
}


def prepare(system, arch):
    version, target, expected = PACKAGES[(system, arch)]
    suffix = "zip" if system == "Windows" else "tgz"
    filename = f"onnxruntime-{target}-{version}.{suffix}"
    cache = ROOT / "src-tauri/target/ai-runtime"
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / filename
    if not archive.exists() or hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
        url = f"https://github.com/microsoft/onnxruntime/releases/download/v{version}/{filename}"
        print(f"Downloading {filename}", flush=True)
        partial = archive.with_suffix(".partial")
        with urllib.request.urlopen(url, timeout=60) as response, partial.open("wb") as output:
            shutil.copyfileobj(response, output)
        if hashlib.sha256(partial.read_bytes()).hexdigest() != expected:
            raise RuntimeError(f"SHA-256 mismatch: {filename}")
        partial.replace(archive)
    output = ROOT / "src-tauri/binaries/onnxruntime" / arch
    output.mkdir(parents=True, exist_ok=True)
    if suffix == "zip":
        with zipfile.ZipFile(archive) as source:
            files = {Path(n).name: source.read(n) for n in source.namelist()
                     if n.endswith(".dll") or Path(n).name in {"LICENSE", "ThirdPartyNotices.txt"}}
    else:
        with tarfile.open(archive) as source:
            files = {}
            for member in source.getmembers():
                name = Path(member.name).name
                if "/lib/" in member.name and "/" in member.name.split("/lib/")[1]:
                    continue
                if name in {"LICENSE", "ThirdPartyNotices.txt"} or name.startswith("libonnxruntime") and (".so" in name or name.endswith(".dylib")):
                    stream = source.extractfile(member)
                    if stream:
                        files[name] = stream.read()
    required = "onnxruntime.dll" if system == "Windows" else "libonnxruntime.dylib" if system == "Darwin" else "libonnxruntime.so"
    if required not in files or "LICENSE" not in files or "ThirdPartyNotices.txt" not in files:
        raise RuntimeError(f"Incomplete official runtime archive: {filename}")
    for name, content in files.items():
        with tempfile.NamedTemporaryFile(dir=output, delete=False) as temporary:
            temporary.write(content)
            staging = Path(temporary.name)
        staging.replace(output / name)
        if name.endswith(".dylib"):
            subprocess.run(["codesign", "--force", "--sign", "-", str(output / name)], check=True)
    print(f"Prepared CPU runtime {version}: {system} {arch}", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--universal", action="store_true", help="prepare both macOS architectures")
    args = parser.parse_args()
    system = platform.system()
    arch = "aarch64" if platform.machine().lower() in {"arm64", "aarch64"} else "x86_64"
    if args.universal and system != "Darwin":
        parser.error("--universal is only supported on macOS")
    for target_arch in ["aarch64", "x86_64"] if args.universal else [arch]:
        prepare(system, target_arch)
