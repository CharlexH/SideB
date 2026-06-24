#!/usr/bin/env bash
set -euo pipefail

bin_path="${1:-}"
repo_root=$(cd -- "$(dirname "$0")/.." && pwd)
manifest_path="${SIDEB_FFMPEG_MANIFEST_PATH:-$repo_root/packaging/shared/LICENSES/THIRD_PARTY_SOURCES.md}"

if [ -z "$bin_path" ]; then
  echo "Usage: $0 <ffmpeg-binary>" >&2
  exit 1
fi

if [ ! -x "$bin_path" ]; then
  echo "ERROR: $bin_path is missing or not executable" >&2
  exit 1
fi

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

ffmpeg_manifest_sha() {
  awk '
    $0 == "### FFmpeg" { in_ffmpeg = 1; next }
    in_ffmpeg && /^### / { in_ffmpeg = 0 }
    in_ffmpeg && /Bundled binary SHA256:/ {
      split($0, parts, "`")
      print parts[2]
      exit
    }
  ' "$manifest_path"
}

if [ "${SIDEB_SKIP_FFMPEG_MANIFEST_CHECK:-0}" != "1" ]; then
  if [ ! -f "$manifest_path" ]; then
    echo "ERROR: FFmpeg third-party manifest is missing: $manifest_path" >&2
    exit 1
  fi

  expected_sha=$(ffmpeg_manifest_sha)
  if [ -z "$expected_sha" ]; then
    echo "ERROR: FFmpeg bundled SHA256 is missing from $manifest_path" >&2
    exit 1
  fi

  actual_sha=$(sha256_file "$bin_path")
  if [ "$actual_sha" != "$expected_sha" ]; then
    echo "ERROR: $bin_path SHA256 $actual_sha does not match FFmpeg manifest SHA256 $expected_sha" >&2
    exit 1
  fi
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
