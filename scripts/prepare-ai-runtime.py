#!/usr/bin/env python3
"""Fetch pinned official CPU runtimes; no Python is needed by the shipped app."""
import argparse
import hashlib
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
# The official NuGet CPU packages include native libraries; .NET is not required.
# ort uses C API 22. Intel macOS's last upstream native package is 1.22.0.
PACKAGES = {
    ("Linux", "x86_64"): ("1.24.2", "linux-x64"),
    ("Windows", "x86_64"): ("1.24.2", "win-x64"),
    ("Darwin", "aarch64"): ("1.24.2", "osx-arm64"),
    ("Darwin", "x86_64"): ("1.22.0", "osx-x64"),
}
DIGESTS = {
    "1.24.2": "782564d3d68e87269ea1812cc94710ad06fe014faf093f01a6f89d0bd58719d3",
    "1.22.0": "d571e63a2329baacb713f441e65ad75284de354db6e1ac435fe4bebbb417986a",
}


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def extract_runtime(archive, target):
    prefix = f"runtimes/{target}/native/"
    with zipfile.ZipFile(archive) as source:
        files = {Path(name).name: source.read(name) for name in source.namelist()
                 if name in {"LICENSE", "ThirdPartyNotices.txt"}
                 or name.startswith(prefix) and "/" not in name[len(prefix):]
                 and name.endswith((".dll", ".so", ".dylib"))}
    required = "onnxruntime.dll" if target.startswith("win-") else "libonnxruntime.dylib" if target.startswith("osx-") else "libonnxruntime.so"
    if not {required, "LICENSE", "ThirdPartyNotices.txt"} <= files.keys():
        raise RuntimeError(f"Incomplete official runtime package for {target}: {archive}")
    return files


def prepare(system, arch):
    version, target = PACKAGES[(system, arch)]
    expected = DIGESTS[version]
    filename = f"microsoft.ml.onnxruntime.{version}.nupkg"
    cache = ROOT / "src-tauri/target/ai-runtime"
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / filename
    if not archive.exists() or digest(archive) != expected:
        url = f"https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/{version}/{filename}"
        print(f"Downloading {filename}", flush=True)
        partial = archive.with_suffix(".partial")
        with urllib.request.urlopen(url, timeout=120) as response, partial.open("wb") as output:
            shutil.copyfileobj(response, output)
        if digest(partial) != expected:
            raise RuntimeError(f"SHA-256 mismatch: {filename}")
        partial.replace(archive)
    files = extract_runtime(archive, target)
    output = ROOT / "src-tauri/binaries/onnxruntime" / arch
    output.mkdir(parents=True, exist_ok=True)
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
