#!/usr/bin/env bash
set -euo pipefail

bin_path="${1:-}"

if [ -z "$bin_path" ]; then
  echo "Usage: $0 <ffmpeg-binary>" >&2
  exit 1
fi

if [ ! -x "$bin_path" ]; then
  echo "ERROR: $bin_path is missing or not executable" >&2
  exit 1
fi

if "$bin_path" -version >/dev/null 2>&1; then
  if ! "$bin_path" -encoders 2>/dev/null | grep -q 'libmp3lame'; then
    echo "ERROR: $bin_path does not provide libmp3lame, which SideB needs for MP3 download transcoding" >&2
    exit 1
  fi

  if ! "$bin_path" -encoders 2>/dev/null | grep -q 'pcm_s16le'; then
    echo "ERROR: $bin_path does not provide the pcm_s16le encoder, which SideB needs for local playback PCM output" >&2
    exit 1
  fi

  if ! "$bin_path" -muxers 2>/dev/null | grep -q 's16le'; then
    echo "ERROR: $bin_path does not provide the s16le muxer, which SideB needs for local playback PCM output" >&2
    exit 1
  fi

  if ! "$bin_path" -h muxer=s16le >/dev/null 2>&1; then
    echo "ERROR: $bin_path cannot open the s16le muxer, which SideB uses with -f s16le" >&2
    exit 1
  fi

  echo "OK: $bin_path provides libmp3lame, pcm_s16le, and s16le for SideB audio paths"
  exit 0
fi

if ! file "$bin_path" | grep -Eq 'ELF|Mach-O'; then
  echo "ERROR: $bin_path could not be inspected as a native binary artifact" >&2
  exit 1
fi

strings_dump="$(mktemp)"
trap 'rm -f "$strings_dump"' EXIT
strings -a "$bin_path" >"$strings_dump"

if ! grep -q 'libmp3lame' "$strings_dump"; then
  echo "ERROR: $bin_path does not advertise libmp3lame, which SideB needs for MP3 download transcoding" >&2
  exit 1
fi

if ! grep -Eq -- '--enable-encoder=[^[:space:]]*pcm_s16le' "$strings_dump"; then
  echo "ERROR: $bin_path does not advertise the pcm_s16le encoder, which SideB needs for local playback PCM output" >&2
  exit 1
fi

if ! grep -Eq -- '--enable-muxer=[^[:space:]]*pcm_s16le' "$strings_dump"; then
  echo "ERROR: $bin_path does not advertise the pcm_s16le muxer component, which SideB needs for local playback PCM output" >&2
  exit 1
fi

if ! grep -Eq 'ffmpeg version|configuration:' "$strings_dump"; then
  echo "ERROR: $bin_path does not look like an FFmpeg-compatible binary" >&2
  exit 1
fi

echo "OK: $bin_path advertises libmp3lame, pcm_s16le, and s16le for SideB audio paths"
