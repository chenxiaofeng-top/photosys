# HEIF preview fixtures

- `portrait-grid.heic` was generated from a synthetic color pattern using libheif and x265. It contains a 2048 × 1024 HEVC grid with red and blue tiles, a 90° clockwise display rotation, and a synthetic EXIF capture time with a UTC offset.
- `portrait-reference.jpg` was encoded from the same synthetic pixels after rotation. It is used to test matching across formats.
- `portrait-48mp-grid.heic` reuses the same synthetic HEVC tiles in an 8 × 6 checkerboard, cropped to 8064 × 6048 with the original 90° clockwise rotation (display size 6048 × 8064). Regenerate it with `python3 scripts/generate-heic-regression-fixture.py`. It reproduces MJPEG optimal-Huffman allocation failures under the application's 256 MiB single-allocation limit; standard Huffman tables decode it without relaxing that limit. No personal photos are included.

# Video preview fixtures

- `video-preview.mp4` is a short H.264 clip with a red first frame and blue later frames. Its index is at the end.
- `video-rotated.mov` is a red and blue H.264 frame with a 90° display rotation.
- `video-preview.mkv`, `video-preview.webm`, and `video-preview.3gp` contain synthetic MPEG-4, VP9, and H.263 color frames, respectively.
- `video-audio-only.mp4` contains a silent AAC track and no video track.
