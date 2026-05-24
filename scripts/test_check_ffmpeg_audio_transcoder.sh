#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname "$0")/.." && pwd)
check_script="$repo_root/scripts/check_ffmpeg_audio_transcoder.sh"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

make_fake_elf() {
  local path="$1"
  local config="$2"
  {
    printf '\177ELF\002\001\001'
    printf '\0%.0s' {1..32}
    printf 'ffmpeg version 7.1.1\n'
    printf 'configuration: %s\n' "$config"
    printf 'libmp3lame\n'
  } >"$path"
  chmod +x "$path"
}

missing_pcm="$tmpdir/ffmpeg-lite-missing-pcm"
make_fake_elf "$missing_pcm" "--enable-libmp3lame --enable-muxer=mp3,mov,mp4,ipod,pcm_s16le --enable-encoder=libmp3lame"
if "$check_script" "$missing_pcm" >/dev/null 2>&1; then
  echo "ERROR: checker accepted a binary without the pcm_s16le encoder" >&2
  exit 1
fi

wrong_muxer="$tmpdir/ffmpeg-lite-wrong-muxer"
make_fake_elf "$wrong_muxer" "--enable-libmp3lame --enable-muxer=mp3,mov,mp4,ipod,s16le --enable-encoder=libmp3lame,pcm_s16le"
if "$check_script" "$wrong_muxer" >/dev/null 2>&1; then
  echo "ERROR: checker accepted the invalid s16le configure component instead of pcm_s16le" >&2
  exit 1
fi

valid="$tmpdir/ffmpeg-lite-valid"
make_fake_elf "$valid" "--enable-libmp3lame --enable-muxer=mp3,mov,mp4,ipod,pcm_s16le --enable-encoder=libmp3lame,pcm_s16le"
"$check_script" "$valid" >/dev/null

echo "OK: checker rejects missing pcm_s16le encoder, rejects the invalid muxer component, and accepts the expected audio contract"
