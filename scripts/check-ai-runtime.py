#!/usr/bin/env python3
"""Load the bundled native CPU runtime and verify C API 22 without a model."""
import argparse
import ctypes
from pathlib import Path
import platform

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("directory", type=Path, help="package extraction directory or runtime resource directory")
args = parser.parse_args()
arch = "aarch64" if platform.machine().lower() in {"arm64", "aarch64"} else "x86_64"
name = "onnxruntime.dll" if platform.system() == "Windows" else "libonnxruntime.dylib" if platform.system() == "Darwin" else "libonnxruntime.so"
matches = [p for p in args.directory.rglob(name) if p.parent.name == arch]
if len(matches) != 1:
    parser.error(f"Expected exactly one {arch}/{name}; found {matches}")
runtime = ctypes.CDLL(str(matches[0].resolve()))
GetApi = ctypes.CFUNCTYPE(ctypes.c_void_p, ctypes.c_uint)
GetVersion = ctypes.CFUNCTYPE(ctypes.c_char_p)


class ApiBase(ctypes.Structure):
    _fields_ = [("get_api", GetApi), ("get_version", GetVersion)]


runtime.OrtGetApiBase.restype = ctypes.POINTER(ApiBase)
base = runtime.OrtGetApiBase().contents
if not base.get_api(22):
    raise RuntimeError("Bundled ONNX Runtime does not support C API 22")
print(f"Loaded {arch} ONNX Runtime {base.get_version().decode()}: {matches[0]}")
