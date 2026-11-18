#!/usr/bin/env python3
"""Expand the existing synthetic HEIF grid without re-encoding its HEVC tiles."""

from pathlib import Path
import struct


def find_box(data, start, end, kind):
    while start < end:
        size, name = struct.unpack_from(">I4s", data, start)
        assert size >= 8 and start + size <= end
        if name == kind:
            return start, start + size
        start += size
    raise ValueError(f"Missing {kind!r} box")


def generate(source):
    data = bytearray(source)
    meta, meta_end = find_box(data, 0, len(data), b"meta")
    idat, idat_end = find_box(data, meta + 12, meta_end, b"idat")
    assert idat_end - idat == 16
    # 6 rows x 8 columns of 1024-pixel tiles, cropped to a 48 MP photo.
    data[idat + 8 : idat_end] = struct.pack(">BBBBHH", 0, 0, 5, 7, 8064, 6048)

    iprp, iprp_end = find_box(data, meta + 12, meta_end, b"iprp")
    ipco, ipco_end = find_box(data, iprp + 8, iprp_end, b"ipco")
    ispe, _ = find_box(data, ipco + 8, ipco_end, b"ispe")
    struct.pack_into(">II", data, ispe + 12, 8064, 6048)

    iref, iref_end = find_box(data, meta + 12, meta_end, b"iref")
    dimg, dimg_end = find_box(data, iref + 12, iref_end, b"dimg")
    assert data[dimg + 8 : dimg_end] == struct.pack(">4H", 1, 2, 2, 3)
    # Alternate the original red and blue tiles. Keep the existing irot/EXIF.
    tiles = [2 + (x + y) % 2 for y in range(6) for x in range(8)]
    replacement = struct.pack(">I4sHH48H", 108, b"dimg", 1, 48, *tiles)
    delta = len(replacement) - (dimg_end - dimg)

    iloc, _ = find_box(data, meta + 12, meta_end, b"iloc")
    # This fixture uses iloc v1 with 32-bit base/extent offsets and lengths.
    assert data[iloc + 8 : iloc + 14] == bytes.fromhex("010000004440")
    count = struct.unpack_from(">H", data, iloc + 14)[0]
    position = iloc + 16
    for _ in range(count):
        _, method, _, base, extents = struct.unpack_from(">HHHIH", data, position)
        if method == 0:  # Absolute mdat offsets move when dimg grows.
            struct.pack_into(">I", data, position + 6, base + delta)
        else:
            assert method == 1  # idat-relative offsets remain unchanged.
        position += 12 + extents * 8

    for offset, end in ((meta, meta_end), (iref, iref_end)):
        struct.pack_into(">I", data, offset, end - offset + delta)
    data[dimg:dimg_end] = replacement
    return data


if __name__ == "__main__":
    fixtures = Path(__file__).resolve().parents[1] / "src-tauri/tests/fixtures"
    (fixtures / "portrait-48mp-grid.heic").write_bytes(
        generate((fixtures / "portrait-grid.heic").read_bytes())
    )
