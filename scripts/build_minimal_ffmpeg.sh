#!/usr/bin/env bash
# Build a minimal ffmpeg for TrimUI Brick (aarch64-linux-musl)
# Only includes audio codecs needed by yt-dlp for MP3 transcoding
#
# Prerequisites:
#   - musl cross-compiler (aarch64-linux-musl-gcc)
#   - libmp3lame source or prebuilt for aarch64-musl
#
# Expected output: ~2-5MB static binary vs 30MB+ full build

set -euo pipefail

FFMPEG_VERSION="${FFMPEG_VERSION:-7.0}"
TARGET="${TARGET:-aarch64-linux-musl}"
PREFIX="${PREFIX:-/tmp/ffmpeg-build}"
OUTPUT_DIR="${OUTPUT_DIR:-$(dirname "$0")/../package/SideB.pak}"

cat <<EOF
=== Minimal FFmpeg Build Configuration ===

This script provides the ./configure flags for a minimal audio-only FFmpeg build.
You need to cross-compile this yourself with your toolchain.

Target: $TARGET
FFmpeg version: $FFMPEG_VERSION

=== Recommended ./configure flags ===

./configure \\
  --prefix="$PREFIX" \\
  --cross-prefix=aarch64-linux-musl- \\
  --arch=aarch64 \\
  --target-os=linux \\
  --enable-cross-compile \\
  --enable-static \\
  --disable-shared \\
  --disable-debug \\
  --disable-doc \\
  --disable-htmlpages \\
  --disable-manpages \\
  --disable-podpages \\
  --disable-txtpages \\
  --disable-network \\
  --disable-autodetect \\
  --disable-programs \\
  --enable-ffmpeg \\
  --disable-ffplay \\
  --disable-ffprobe \\
  --disable-avdevice \\
  --disable-swscale \\
  --disable-postproc \\
  --disable-everything \\
  --enable-protocol=file \\
  --enable-protocol=pipe \\
  --enable-demuxer=mp3 \\
  --enable-demuxer=aac \\
  --enable-demuxer=ogg \\
  --enable-demuxer=webm \\
  --enable-demuxer=matroska \\
  --enable-demuxer=mov \\
  --enable-demuxer=flac \\
  --enable-demuxer=wav \\
  --enable-decoder=mp3 \\
  --enable-decoder=mp3float \\
  --enable-decoder=aac \\
  --enable-decoder=vorbis \\
  --enable-decoder=opus \\
  --enable-decoder=flac \\
  --enable-decoder=pcm_s16le \\
  --enable-decoder=pcm_s24le \\
  --enable-muxer=mp3 \\
  --enable-encoder=libmp3lame \\
  --enable-libmp3lame \\
  --enable-filter=aresample \\
  --enable-filter=anull \\
  --extra-cflags="-Os -ffunction-sections -fdata-sections" \\
  --extra-ldflags="-Wl,--gc-sections -static"

=== After building ===

Strip the binary:
  aarch64-linux-musl-strip ffmpeg

Copy to SideB package:
  cp ffmpeg "$OUTPUT_DIR/ffmpeg"

Verify libmp3lame support:
  ./scripts/check_ffmpeg_audio_transcoder.sh "$OUTPUT_DIR/ffmpeg"

EOF
