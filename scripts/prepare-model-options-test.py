#!/usr/bin/env python3
"""Download and verify all five pinned CPU model packages for opt-in native tests.
No user photos are accessed. Python is a developer/CI dependency only.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
from pathlib import Path
import time
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("directory", type=Path)
parser.add_argument("--model-id", action="append", help="download selected IDs instead of all five")
args = parser.parse_args()
catalog = json.loads((Path(__file__).parent / "ai-model-catalog.json").read_text())
if args.model_id and set(args.model_id) - {m["id"] for m in catalog}:
    parser.error("Unknown model ID")


def download(model):
    directory = args.directory / model["id"]
    directory.mkdir(parents=True, exist_ok=True)
    for asset in model["assets"]:
        path = directory / asset["name"]
        def valid():
            if not path.is_file() or path.stat().st_size != asset["size"]:
                return False
            with path.open("rb") as stream:
                return hashlib.file_digest(stream, "sha256").hexdigest() == asset["sha"]
        if valid():
            print("Verified cached", model["id"], asset["name"], flush=True)
            continue
        url = f'https://huggingface.co/{asset["repository"]}/resolve/{asset["revision"]}/{asset["path"]}'
        for attempt in range(3):
            try:
                offset = path.stat().st_size if path.exists() else 0
                if offset >= asset["size"]:
                    path.unlink()
                    offset = 0
                request = urllib.request.Request(url, headers={"Range": f"bytes={offset}-"} if offset else {})
                with urllib.request.urlopen(request, timeout=90) as response:
                    append = response.status == 206
                    if append and response.headers.get("Content-Range") != f'bytes {offset}-{asset["size"] - 1}/{asset["size"]}':
                        raise RuntimeError("Invalid resume range")
                    if response.status not in (200, 206):
                        raise RuntimeError("Unexpected download status")
                    with path.open("ab" if append else "wb") as stream:
                        while chunk := response.read(1024 * 1024):
                            stream.write(chunk)
                            if stream.tell() > asset["size"]:
                                raise RuntimeError("Oversized model")
                if not valid():
                    path.unlink(missing_ok=True)
                    raise RuntimeError("Model size or SHA-256 mismatch")
                print("Verified", model["id"], asset["name"], flush=True)
                break
            except Exception:
                if attempt == 2:
                    raise
                time.sleep(1 + attempt)


with ThreadPoolExecutor(max_workers=2) as pool:
    list(pool.map(download, [m for m in catalog if not args.model_id or m["id"] in args.model_id]))
