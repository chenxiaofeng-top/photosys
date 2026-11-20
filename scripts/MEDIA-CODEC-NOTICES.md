# PhotoSys media codec

PhotoSys runs FFmpeg for media previews and FFprobe for media details. Both
executables use FFmpeg 8.1.3 under the GNU Lesser General Public License,
version 2.1 or later. The build includes libvpx 1.17.0, libvorbis 1.3.7 and
libogg 1.3.6 under their respective permissive licenses.

The `media-codec-sources.tar.gz` release artifact contains the exact,
unmodified upstream sources and the build script. It is published alongside
the PhotoSys installers in the same Desktop builds run. The FFmpeg
configuration is also available via `photosys-ffmpeg -buildconf`.

Upstream projects:

- FFmpeg — https://ffmpeg.org/
- libvpx — https://chromium.googlesource.com/webm/libvpx/
- libogg and libvorbis — https://www.xiph.org/
