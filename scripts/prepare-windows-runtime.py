#!/usr/bin/env python3
"""Stage the Microsoft x64 C++ runtime DLLs for the Windows installer."""
import os
from pathlib import Path
import platform
import shutil
import subprocess


ROOT = Path(__file__).resolve().parents[1]
DLLS = ("msvcp140.dll", "msvcp140_1.dll", "vcruntime140.dll", "vcruntime140_1.dll")
REDIST_COMPONENT = "Microsoft.VisualStudio.Component.VC.Redist.14.Latest"


def find_crt():
    vswhere = Path(os.environ["ProgramFiles(x86)"]) / "Microsoft Visual Studio/Installer/vswhere.exe"
    if not vswhere.is_file():
        raise RuntimeError("Install Visual Studio Build Tools with the x64 C++ redistributable component.")

    installation = subprocess.check_output([
        str(vswhere), "-latest", "-products", "*", "-requires", REDIST_COMPONENT,
        "-property", "installationPath", "-utf8",
    ], encoding="utf-8").strip()
    if not installation:
        raise RuntimeError(f"Add {REDIST_COMPONENT} in Visual Studio Installer.")

    redist = Path(installation) / "VC/Redist/MSVC"
    versions = [path for path in redist.glob("*.*") if path.is_dir()
                and all(part.isdecimal() for part in path.name.split("."))]
    if not versions:
        raise RuntimeError(f"No versioned redistributable directory found under {redist}.")
    latest = max(versions, key=lambda path: tuple(map(int, path.name.split("."))))
    directories = sorted(path for path in latest.glob("x64/Microsoft.VC*.CRT") if path.is_dir())
    if len(directories) != 1:
        raise RuntimeError(f"Expected one x64 CRT directory under {latest}, found {directories}.")
    return directories[0]


def stage_crt(source, output):
    missing = [name for name in DLLS if not (source / name).is_file() or not (source / name).stat().st_size]
    if missing:
        raise RuntimeError(f"Incomplete x64 CRT in {source}: {', '.join(missing)}")
    output.mkdir(parents=True, exist_ok=True)
    for name in DLLS:
        shutil.copyfile(source / name, output / name)


if __name__ == "__main__":
    if platform.system() != "Windows" or platform.machine().lower() not in {"amd64", "x86_64"}:
        raise SystemExit("This script prepares the runtime for Windows x64 builds only.")
    try:
        source = find_crt()
        stage_crt(source, ROOT / "src-tauri/target/windows-runtime/x64")
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error
    print(f"Prepared Windows x64 C++ runtime from {source}")
