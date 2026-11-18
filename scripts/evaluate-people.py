#!/usr/bin/env python3
"""Calibrate and evaluate the actual Rust face/grouping pipeline on labeled local photos.
Writes an accuracy acceptance report; does not modify runtime grouping policies.
"""
import argparse
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("models", type=Path)
parser.add_argument("manifest", type=Path)
parser.add_argument("output", type=Path)
args = parser.parse_args()
env = dict(os.environ)
env.update(PHOTOSYS_PEOPLE_MODEL_DIR=str(args.models.resolve()),
           PHOTOSYS_PEOPLE_EVALUATION=str(args.manifest.resolve()),
           PHOTOSYS_PEOPLE_EVALUATION_OUTPUT=str(args.output.resolve()))
subprocess.run(["cargo", "test", "--locked", "--manifest-path", str(Path(__file__).resolve().parents[1] / "src-tauri/Cargo.toml"),
                "evaluate_labeled_people", "--", "--ignored", "--nocapture"], env=env, check=True)
