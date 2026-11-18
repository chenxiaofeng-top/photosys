#!/usr/bin/env python3
"""Stage desktop artifacts, upload to a draft, then publish a complete release."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess


def stage_assets(artifacts, output):
    patterns = [
        ("PhotoSys-windows-x64", "*.exe"),
        ("PhotoSys-linux-x64", "*.deb"),
        ("PhotoSys-linux-x64", "*.AppImage"),
        ("PhotoSys-macos-universal", "*.dmg"),
        ("PhotoSys-linux-x64", "media-codec-sources.tar.gz"),
    ]
    sources = []
    for artifact, pattern in patterns:
        matches = sorted(p for p in (artifacts / artifact).rglob(pattern) if p.is_file())
        if len(matches) != 1 or matches[0].stat().st_size == 0:
            raise ValueError(f"Expected exactly one nonempty {artifact}/{pattern}; got {matches}")
        sources.append(matches[0])
    if len({p.name for p in sources}) != len(sources):
        raise ValueError("Release asset filenames must be unique")
    output.mkdir(parents=True, exist_ok=True)
    assets = []
    for source in sources:
        destination = output / source.name
        shutil.copyfile(source, destination)
        assets.append(destination)
    checksums = output / "SHA256SUMS"
    with checksums.open("w", encoding="utf-8", newline="\n") as stream:
        for asset in sorted(assets):
            with asset.open("rb") as content:
                digest = hashlib.file_digest(content, "sha256").hexdigest()
            stream.write(f"{digest}  {asset.name}\n")
    return assets + [checksums]


def gh(*args):
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout


def get_release(repo, tag):
    try:
        # The REST tag endpoint only finds published releases. gh also looks up
        # pending draft tags through GraphQL, which is required for retries.
        release = json.loads(gh("release", "view", tag, "--repo", repo,
                                "--json", "isDraft,url,assets"))
        return {"draft": release["isDraft"], "html_url": release["url"],
                "assets": release["assets"]}
    except subprocess.CalledProcessError as error:
        if error.stderr.strip() == "release not found":
            return None
        raise


def publish(repo, tag, assets):
    release = get_release(repo, tag)
    if release is not None and not release["draft"]:
        print(f"Release {tag} is already public; leaving it unchanged.", flush=True)
        return release["html_url"], False
    if release is None:
        gh("release", "create", tag, "--repo", repo, "--verify-tag", "--draft",
           "--title", f"PhotoSys {tag}", "--generate-notes")
    # A failed upload leaves the release in draft; retry replaces partial assets.
    gh("release", "upload", tag, *map(str, assets), "--repo", repo, "--clobber")
    release = get_release(repo, tag)
    expected = {p.name: p.stat().st_size for p in assets}
    if release is None or not release["draft"]:
        raise ValueError("Release must still be a draft before publishing")
    actual = {asset["name"]: asset["size"] for asset in release["assets"]
              if asset.get("state") == "uploaded"}
    if actual != expected or len(release["assets"]) != len(expected):
        raise ValueError(f"Uploaded release assets do not match: expected {expected}, got {actual}; keeping draft")
    gh("release", "edit", tag, "--repo", repo, "--draft=false")
    release = get_release(repo, tag)
    if release is None or release["draft"]:
        raise ValueError("Release is not public after publishing; check the release state on GitHub")
    return release["html_url"], True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("artifacts", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    if not re.fullmatch(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", args.tag, flags=re.ASCII):
        parser.error("Expected a stable release tag such as v0.1.1")
    assets = stage_assets(args.artifacts, args.output)
    url, published = publish(os.environ["GITHUB_REPOSITORY"], args.tag, assets)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as stream:
            stream.write(f"### GitHub Release\n\n[{args.tag}]({url})\n\n")
            if published:
                stream.writelines(f"- `{asset.name}`\n" for asset in assets)
            else:
                stream.write("Already public; existing assets were not changed.\n")
    print(url)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr or str(error)) from error
