#!/usr/bin/env python3
"""Install the pinned, checksum-verified PMTiles sidecar for Tauri desktop builds."""
import argparse
import hashlib
import io
import json
import os
import pathlib
import platform
import subprocess
import tarfile
import time
import urllib.request
import zipfile

ROOT=pathlib.Path(__file__).resolve().parents[1]
VERSION='1.31.2'
ARTIFACTS={
 ('Linux','x86_64'):('go-pmtiles_1.31.2_Linux_x86_64.tar.gz','3ed7dbf4ec2e6dfe5e25b6f70d1ffc932729f93c86db353bf514dd71010a312f'),
 ('Linux','arm64'):('go-pmtiles_1.31.2_Linux_arm64.tar.gz','f8bd47e7ea866863489cad588fbaf2f31f42e5821f7a03f009b3769f05801cb1'),
 ('Darwin','x86_64'):('go-pmtiles-1.31.2_Darwin_x86_64.zip','1f0dc02eee6c58312dd6c509faee1b5c32f0596568af1bf51f1b034e7a88a65b'),
 ('Darwin','arm64'):('go-pmtiles-1.31.2_Darwin_arm64.zip','40528f7f616fcbf91207cd48c8fc023d213f6d86c0cbf1f748732803d1880f3d'),
 ('Windows','x86_64'):('go-pmtiles_1.31.2_Windows_x86_64.zip','a658baa4d7e55020aef6ca17bd9ff9faa1582671266b36f58c52db0ac8e785a1'),
 ('Windows','arm64'):('go-pmtiles_1.31.2_Windows_arm64.zip','8780a17453c63af757917a694cbbb50b943db89cc3f1b07e6fd62c1ff8e6963b'),
}


def download(url):
    for attempt in range(3):
        try:
            request=urllib.request.Request(url,headers={'User-Agent':'PhotoSys-offline-map-build'})
            with urllib.request.urlopen(request,timeout=120) as response: return response.read()
        except (OSError,urllib.error.URLError):
            if attempt==2: raise
            time.sleep(1)


def prepare(system,arch):
    filename,digest=ARTIFACTS[(system,arch)]
    cache=ROOT/'src-tauri/target/offline-map-tools';cache.mkdir(parents=True,exist_ok=True)
    archive=cache/filename
    if not archive.exists() or hashlib.sha256(archive.read_bytes()).hexdigest()!=digest:
        raw=download(f'https://github.com/protomaps/go-pmtiles/releases/download/v{VERSION}/{filename}')
        if hashlib.sha256(raw).hexdigest()!=digest: raise ValueError(f'Checksum mismatch: {filename}')
        archive.write_bytes(raw)
    raw=archive.read_bytes()
    executable='pmtiles.exe' if system=='Windows' else 'pmtiles'
    if filename.endswith('.zip'):
        with zipfile.ZipFile(io.BytesIO(raw)) as source:
            name=next(n for n in source.namelist() if pathlib.PurePosixPath(n).name==executable)
            binary=source.read(name)
    else:
        with tarfile.open(fileobj=io.BytesIO(raw),mode='r:gz') as source:
            member=next(m for m in source if pathlib.PurePosixPath(m.name).name==executable and m.isfile())
            binary=source.extractfile(member).read()
    target_arch='aarch64' if arch=='arm64' else 'x86_64'
    suffix={'Linux':'unknown-linux-gnu','Windows':'pc-windows-msvc','Darwin':'apple-darwin'}[system]
    output=ROOT/f'src-tauri/binaries/photosys-pmtiles-{target_arch}-{suffix}'
    if system=='Windows': output=output.with_suffix('.exe')
    output.parent.mkdir(parents=True,exist_ok=True);output.write_bytes(binary);output.chmod(0o755)
    return output


def main():
    parser=argparse.ArgumentParser();parser.add_argument('--universal',action='store_true');args=parser.parse_args()
    system=platform.system();arch=platform.machine().lower()
    arch='arm64' if arch in ['aarch64','arm64'] else 'x86_64' if arch in ['amd64','x86_64'] else arch
    if args.universal:
        if system!='Darwin': parser.error('--universal requires macOS')
        paths=[prepare('Darwin',a) for a in ['arm64','x86_64']]
        output=ROOT/'src-tauri/binaries/photosys-pmtiles-universal-apple-darwin'
        subprocess.run(['lipo','-create',*map(str,paths),'-output',str(output)],check=True)
    else: output=prepare(system,arch)
    print(output)
    # Static assets are committed so normal builds do not depend on font/map servers.
    required=[ROOT/'public/maps/chinese.ttf',ROOT/'public/maps/overview.geojson',
              ROOT/'public/maps/fonts/Noto Sans Regular/0-255.pbf',ROOT/'public/maps/sprites/light.png',
              ROOT/'src-tauri/resources/locations/china.json.gz']
    missing=[str(p.relative_to(ROOT)) for p in required if not p.is_file()]
    if missing: raise ValueError('Offline map resources missing: '+', '.join(missing))
    assets=json.loads((ROOT/'public/maps/manifest.json').read_text())
    for relative,digest in assets['files'].items():
        path=ROOT/'public/maps'/relative
        if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest()!=digest:
            raise ValueError('Offline map asset checksum mismatch: '+relative)
    manifest=json.loads((ROOT/'src-tauri/resources/locations/manifest.json').read_text())
    dataset=ROOT/'src-tauri/resources/locations/china.json.gz'
    if hashlib.sha256(dataset.read_bytes()).hexdigest()!=manifest['sha256']:
        raise ValueError('Administrative dataset checksum mismatch')

if __name__=='__main__': main()
