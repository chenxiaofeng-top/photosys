#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cache="${PHOTOSYS_CODEC_CACHE:-$repo/src-tauri/target/media-codec}"
destination="$repo/src-tauri/binaries"
jobs="${PHOTOSYS_CODEC_JOBS:-4}"
mkdir -p "$cache" "$destination"
cache="$(cd "$cache" && pwd)"
sources="$cache/sources"
mkdir -p "$sources"

if [[ "$(uname -s)" == Darwin ]]; then
  # macOS's sha256sum does not accept the GNU stdin-check invocation below.
  sha256=(shasum -a 256)
elif command -v sha256sum >/dev/null; then
  sha256=(sha256sum)
else
  sha256=(shasum -a 256)
fi

download() {
  local name="$1" url="$2" digest="$3" file="$sources/$1"
  if ! (cd "$sources" && printf '%s  %s\n' "$digest" "$name" | "${sha256[@]}" -c) >/dev/null 2>&1; then
    curl --fail --location --silent --show-error --retry 3 --max-time 180 "$url" -o "$file.part"
    (cd "$sources" && printf '%s  %s\n' "$digest" "$name.part" | "${sha256[@]}" -c)
    mv "$file.part" "$file"
  fi
}

ffmpeg_source=ffmpeg-8.1.3.tar.gz
vpx_source=libvpx-1.17.0.tar.gz
ogg_source=libogg-1.3.6.tar.xz
vorbis_source=libvorbis-1.3.7.tar.xz
download "$ffmpeg_source" https://codeload.github.com/FFmpeg/FFmpeg/tar.gz/refs/tags/n8.1.3 \
  09f990289327d3ebfedf4fc15cae3f884a65dab7a5aac7bbb5b01ff83900efd9
download "$vpx_source" https://codeload.github.com/webmproject/libvpx/tar.gz/refs/tags/v1.17.0 \
  1020f184046187baa2985dbde38e0691f49c44088bca7a1842b0236c6081dc0a
download "$ogg_source" https://downloads.xiph.org/releases/ogg/libogg-1.3.6.tar.xz \
  5c8253428e181840cd20d41f3ca16557a9cc04bad4a3d04cce84808677fa1061
download "$vorbis_source" https://downloads.xiph.org/releases/vorbis/libvorbis-1.3.7.tar.xz \
  b33cc4934322bcbf6efcbacf49e3ca01aadbea4114ec9589d1b1e9d20f72954b

case "$(uname -s)" in
  Linux) default_target=x86_64-unknown-linux-gnu ;;
  Darwin) default_target="$(uname -m | sed 's/^arm64$/aarch64/')-apple-darwin" ;;
  MINGW*|MSYS*|UCRT*) default_target=x86_64-pc-windows-msvc ;;
  *) echo "Unsupported host for FFmpeg" >&2; exit 1 ;;
esac

build() (
  local target="$1" arch=x86_64 extension= vpx_target=generic-gnu
  local work="$cache/$target" prefix="$cache/$target/install"
  local cc=gcc cxx=g++
  local cflags="-O2" ldflags=""
  local autotools=() platform=() x86asm=(--disable-x86asm)
  case "$target" in
    x86_64-unknown-linux-gnu)
      if command -v nasm >/dev/null; then vpx_target=x86_64-linux-gcc; fi
      ;;
    aarch64-apple-darwin|x86_64-apple-darwin)
      local apple_arch=x86_64
      if [[ "$target" == aarch64-* ]]; then
        arch=aarch64
        apple_arch=arm64
        export MACOSX_DEPLOYMENT_TARGET=11.0
      else
        export MACOSX_DEPLOYMENT_TARGET=10.15
      fi
      cc=clang
      cxx=clang++
      cflags="$cflags -arch $apple_arch"
      ldflags="-arch $apple_arch"
      autotools=(--host="$target")
      platform=(--target-os=darwin)
      if [[ "$apple_arch" != "$(uname -m)" ]]; then platform+=(--enable-cross-compile); fi
      ;;
    x86_64-pc-windows-msvc)
      extension=.exe
      platform=(--target-os=mingw32)
      ldflags="-static"
      if command -v nasm >/dev/null; then vpx_target=x86_64-win64-gcc; fi
      ;;
    *) echo "Unsupported FFmpeg target $target" >&2; exit 1 ;;
  esac
  if [[ "$arch" == x86_64 ]] && command -v nasm >/dev/null; then
    x86asm=(--enable-x86asm)
  fi

  local signature
  signature="$( "${sha256[@]}" "$repo/scripts/build-media-codec.sh" | cut -d' ' -f1 )"
  if [[ ! -x "$prefix/bin/ffmpeg$extension" || ! -x "$prefix/bin/ffprobe$extension" || ! -f "$work/build-signature" ||
        "$(< "$work/build-signature")" != "$signature" ]]; then
    rm -rf -- "$work"
    mkdir -p "$work"/{ffmpeg,vpx,ogg,vorbis}
    tar -xf "$sources/$ffmpeg_source" -C "$work/ffmpeg" --strip-components=1
    tar -xf "$sources/$vpx_source" -C "$work/vpx" --strip-components=1
    tar -xf "$sources/$ogg_source" -C "$work/ogg" --strip-components=1
    tar -xf "$sources/$vorbis_source" -C "$work/vorbis" --strip-components=1
    if [[ "$target" == *-apple-darwin ]]; then
      # libvorbis 1.3.7 injects a flag removed from recent Apple linkers.
      sed -i.bak 's/ -force_cpusubtype_ALL//g' "$work/vorbis/configure"
    fi
    export PKG_CONFIG_PATH= PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig"
    (
      cd "$work/ogg"
      CC="$cc" CFLAGS="$cflags" LDFLAGS="$ldflags" ./configure \
        --prefix="$prefix" --disable-shared --enable-static "${autotools[@]}"
      make -j "$jobs"
      make install
    )
    (
      cd "$work/vorbis"
      CC="$cc" CFLAGS="$cflags" LDFLAGS="$ldflags" ./configure \
        --prefix="$prefix" --with-ogg="$prefix" --disable-shared --enable-static "${autotools[@]}"
      make -j "$jobs"
      make install
    )
    (
      cd "$work/vpx"
      CC="$cc" CXX="$cxx" CFLAGS="$cflags" CXXFLAGS="$cflags" LDFLAGS="$ldflags" ./configure \
        --prefix="$prefix" --target="$vpx_target" --disable-examples --disable-tools \
        --disable-docs --disable-unit-tests --disable-vp9 --disable-vp8-decoder --disable-shared
      make -j "$jobs"
      make install
    )
    (
      cd "$work/ffmpeg"
      ./configure --prefix="$prefix" --cc="$cc" --arch="$arch" \
        "${platform[@]}" "${x86asm[@]}" --pkg-config-flags=--static \
        --extra-cflags="$cflags" --extra-ldflags="$ldflags" \
        --disable-autodetect --disable-network --disable-doc --disable-ffplay \
        --disable-shared --enable-static --disable-everything --enable-libvpx --enable-libvorbis \
        --enable-protocol=file,fd --enable-demuxer=mov,matroska --enable-muxer=image2,webm \
        --enable-parser=h264,hevc,aac,mjpeg,mpeg4video,h263,vp8,vp9 \
        --enable-decoder=h264,hevc,mjpeg,mpeg4,h263,vp8,vp9,aac,alac,pcm_s16le,pcm_s16be,pcm_s24le,pcm_s24be,pcm_s32le,pcm_s32be,pcm_f32le,pcm_f32be \
        --enable-encoder=mjpeg,libvpx_vp8,libvorbis \
        --enable-filter=scale,format,aformat,aresample,transpose,hflip,vflip,crop,xstack,null,anull,trim,atrim
      make -j "$jobs" ffmpeg$extension ffprobe$extension
      mkdir -p "$prefix/bin"
      install -m 755 "ffmpeg$extension" "ffprobe$extension" "$prefix/bin/"
    )
    printf '%s\n' "$signature" > "$work/build-signature"
  fi

  install -m 755 "$prefix/bin/ffmpeg$extension" "$destination/photosys-ffmpeg-$target$extension"
  install -m 755 "$prefix/bin/ffprobe$extension" "$destination/photosys-ffprobe-$target$extension"
  local notices="$destination/media-codec-licenses"
  mkdir -p "$notices"
  cp "$work/ffmpeg/COPYING.LGPLv2.1" "$notices/FFmpeg-LGPL-2.1.txt"
  cp "$work/ogg/COPYING" "$notices/libogg.txt"
  cp "$work/vorbis/COPYING" "$notices/libvorbis.txt"
  cp "$work/vpx/LICENSE" "$notices/libvpx.txt"
  cp "$work/vpx/PATENTS" "$notices/libvpx-PATENTS.txt"
  cp "$repo/scripts/MEDIA-CODEC-NOTICES.md" "$notices/NOTICE.md"
)

target="${1:-$default_target}"
if [[ "$target" == universal-apple-darwin ]]; then
  build aarch64-apple-darwin
  build x86_64-apple-darwin
  for program in ffmpeg ffprobe; do
    lipo -create "$destination/photosys-$program-aarch64-apple-darwin" \
      "$destination/photosys-$program-x86_64-apple-darwin" \
      -output "$destination/photosys-$program-universal-apple-darwin"
  done
else
  build "$target"
fi

tar -czf "$destination/media-codec-sources.tar.gz" \
  -C "$sources" "$ffmpeg_source" "$vpx_source" "$ogg_source" "$vorbis_source" \
  -C "$repo/scripts" build-media-codec.sh MEDIA-CODEC-NOTICES.md
