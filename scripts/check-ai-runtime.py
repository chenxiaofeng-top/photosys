#!/usr/bin/env python3
"""Check that the bundled ONNX runtimes load and expose C API 22."""
import argparse
from contextlib import nullcontext
import ctypes
import os
from pathlib import Path
import platform

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("directory", type=Path, help="package extraction directory or runtime resource directory")
parser.add_argument("--dll-directory", type=Path, help="Windows directory containing the bundled C++ DLLs")
parser.add_argument("--require-directml", action="store_true", help="verify the Windows x64 DirectML runtime and redistributable")
args = parser.parse_args()
if args.dll_directory is not None and platform.system() != "Windows":
    parser.error("--dll-directory is supported only on Windows")
arch = "aarch64" if platform.machine().lower() in {"arm64", "aarch64"} else "x86_64"
name = "onnxruntime.dll" if platform.system() == "Windows" else "libonnxruntime.dylib" if platform.system() == "Darwin" else "libonnxruntime.so"
if args.require_directml and (platform.system() != "Windows" or arch != "x86_64"):
    parser.error("--require-directml is supported only on Windows x64")
matches = [p for p in args.directory.rglob(name) if p.parent.name == arch]
gpu = [p for p in matches if p.parent.parent.name == "directml"]
matches = [p for p in matches if p not in gpu]
if len(matches) != 1:
    parser.error(f"Expected exactly one {arch}/{name}; found {matches}")
GetApi = ctypes.CFUNCTYPE(ctypes.c_void_p, ctypes.c_uint)
GetVersion = ctypes.CFUNCTYPE(ctypes.c_char_p)


class ApiBase(ctypes.Structure):
    _fields_ = [("get_api", GetApi), ("get_version", GetVersion)]


def check(path, directml=False):
    runtime = ctypes.CDLL(str(path.resolve()))
    runtime.OrtGetApiBase.restype = ctypes.POINTER(ApiBase)
    base = runtime.OrtGetApiBase().contents
    if not base.get_api(22):
        raise RuntimeError(f"C API 22 unavailable in {path}")
    if directml:
        runtime.OrtSessionOptionsAppendExecutionProvider_DML
    provider = "DirectML" if directml else "CPU"
    print(f"Loaded {arch} ONNX Runtime {base.get_version().decode()} ({provider}): {path}")


# Python runs outside the app directory, so it needs the app-local search path explicitly.
dll_directory = (os.add_dll_directory(str(args.dll_directory.resolve()))
                 if args.dll_directory is not None else nullcontext())
with dll_directory:
    check(matches[0])
    if args.require_directml:
        if len(gpu) != 1:
            parser.error(f"Expected exactly one directml/{arch}/{name}; found {gpu}")
        required = {"DirectML.dll", "onnxruntime_providers_shared.dll", "LICENSE",
                    "ThirdPartyNotices.txt", "DirectML-LICENSE.txt",
                    "DirectML-LICENSE-CODE.txt", "DirectML-ThirdPartyNotices.txt"}
        missing = required - {path.name for path in gpu[0].parent.iterdir() if path.is_file()}
        if missing:
            parser.error(f"Missing DirectML package files: {', '.join(sorted(missing))}")
        dependency = ctypes.WinDLL(str(gpu[0].with_name("DirectML.dll").resolve()))
        check(gpu[0], directml=True)
