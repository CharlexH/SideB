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

  echo "OK: $bin_path provides libmp3lame for SideB download transcoding"
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

if ! grep -Eq 'ffmpeg version|configuration:' "$strings_dump"; then
  echo "ERROR: $bin_path does not look like an FFmpeg-compatible binary" >&2
  exit 1
fi

echo "OK: $bin_path advertises libmp3lame for SideB download transcoding"
