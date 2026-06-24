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
if SIDEB_SKIP_FFMPEG_MANIFEST_CHECK=1 "$check_script" "$missing_pcm" >/dev/null 2>&1; then
  echo "ERROR: checker accepted a binary without the pcm_s16le encoder" >&2
  exit 1
fi

wrong_muxer="$tmpdir/ffmpeg-lite-wrong-muxer"
make_fake_elf "$wrong_muxer" "--enable-libmp3lame --enable-muxer=mp3,mov,mp4,ipod,s16le --enable-encoder=libmp3lame,pcm_s16le"
if SIDEB_SKIP_FFMPEG_MANIFEST_CHECK=1 "$check_script" "$wrong_muxer" >/dev/null 2>&1; then
  echo "ERROR: checker accepted the invalid s16le configure component instead of pcm_s16le" >&2
  exit 1
fi

valid="$tmpdir/ffmpeg-lite-valid"
make_fake_elf "$valid" "--enable-libmp3lame --enable-muxer=mp3,mov,mp4,ipod,pcm_s16le --enable-encoder=libmp3lame,pcm_s16le"
SIDEB_SKIP_FFMPEG_MANIFEST_CHECK=1 "$check_script" "$valid" >/dev/null

valid_sha=$(shasum -a 256 "$valid" | awk '{print $1}')
matching_manifest="$tmpdir/THIRD_PARTY_SOURCES.matching.md"
cat >"$matching_manifest" <<EOF
# Third-Party Source Manifest

### FFmpeg

- Bundled binary SHA256: \`$valid_sha\`
EOF

SIDEB_FFMPEG_MANIFEST_PATH="$matching_manifest" "$check_script" "$valid" >/dev/null

mismatched_manifest="$tmpdir/THIRD_PARTY_SOURCES.mismatched.md"
cat >"$mismatched_manifest" <<'EOF'
# Third-Party Source Manifest

### FFmpeg

- Bundled binary SHA256: `0000000000000000000000000000000000000000000000000000000000000000`
EOF

if SIDEB_FFMPEG_MANIFEST_PATH="$mismatched_manifest" "$check_script" "$valid" >/dev/null 2>&1; then
  echo "ERROR: checker accepted an ffmpeg-lite binary whose SHA256 does not match THIRD_PARTY_SOURCES.md" >&2
  exit 1
fi

echo "OK: checker rejects missing pcm_s16le encoder, rejects the invalid muxer component, verifies manifest SHA256, and accepts the expected audio contract"
