#!/usr/bin/env python3
"""Reject release tags that differ from application manifests or lock files."""
import argparse
import json
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def check_versions(root, tag):
    if not re.fullmatch(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", tag, flags=re.ASCII):
        raise ValueError(f"Expected a stable version tag such as v0.1.1; got {tag!r}")
    expected = tag[1:]
    package = json.loads((root / "package.json").read_text(encoding="utf-8"))
    npm_lock = json.loads((root / "package-lock.json").read_text(encoding="utf-8"))
    tauri = json.loads((root / "src-tauri/tauri.conf.json").read_text(encoding="utf-8"))
    cargo = tomllib.loads((root / "src-tauri/Cargo.toml").read_text(encoding="utf-8"))
    cargo_lock = tomllib.loads((root / "src-tauri/Cargo.lock").read_text(encoding="utf-8"))
    locked_app = [p for p in cargo_lock["package"]
                  if p["name"] == cargo["package"]["name"] and "source" not in p]
    if len(locked_app) != 1:
        raise ValueError("Cargo.lock must contain exactly one local application package")
    versions = {
        "package.json": package["version"],
        "package-lock.json (root)": npm_lock["version"],
        'package-lock.json (packages[""])': npm_lock["packages"][""]["version"],
        "src-tauri/tauri.conf.json": tauri["version"],
        "src-tauri/Cargo.toml": cargo["package"]["version"],
        "src-tauri/Cargo.lock (application)": locked_app[0]["version"],
    }
    mismatches = [f"{name}: {version!r}" for name, version in versions.items() if version != expected]
    if mismatches:
        raise ValueError(f"Tag {tag} requires version {expected} everywhere:\n" + "\n".join(mismatches))
    return expected


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    args = parser.parse_args()
    try:
        version = check_versions(ROOT, args.tag)
    except (ValueError, KeyError, OSError) as error:
        parser.exit(1, f"Release version check failed: {error}\n")
    print(f"Release tag and all application versions match: {version}")
