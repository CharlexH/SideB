#!/usr/bin/env bash
set -euo pipefail

repo_root="${1:-$(cd -- "$(dirname "$0")/.." && pwd)}"
cargo_toml="$repo_root/spotify-ui-rs/Cargo.toml"
pak_json="$repo_root/pak.json"
readme="$repo_root/README.md"

read_required_line() {
  local pattern="$1"
  local path="$2"
  local label="$3"
  local value

  value=$(sed -n "$pattern" "$path" | head -n 1)
  if [ -z "$value" ]; then
    echo "ERROR: failed to read $label from $path" >&2
    exit 1
  fi

  printf '%s\n' "$value"
}

version=$(read_required_line 's/^version = "\(.*\)"/\1/p' "$cargo_toml" "Cargo version")
expected_tag="v$version"
expected_release_filename="SideB-${version}-nextui.zip"

pak_version=$(read_required_line 's/^[[:space:]]*"version":[[:space:]]*"\([^"]*\)".*/\1/p' "$pak_json" "pak.json version")
pak_release_filename=$(read_required_line 's/^[[:space:]]*"release_filename":[[:space:]]*"\([^"]*\)".*/\1/p' "$pak_json" "pak.json release_filename")

if [ "$pak_version" != "$expected_tag" ]; then
  echo "ERROR: pak.json version ($pak_version) does not match $expected_tag" >&2
  exit 1
fi

if [ "$pak_release_filename" != "$expected_release_filename" ]; then
  echo "ERROR: pak.json release_filename ($pak_release_filename) does not match $expected_release_filename" >&2
  exit 1
fi

if ! grep -F "\"$expected_tag\":" "$pak_json" >/dev/null; then
  echo "ERROR: pak.json changelog is missing $expected_tag" >&2
  exit 1
fi

if ! grep -F "Latest release: \`$expected_tag\`" "$readme" >/dev/null; then
  echo "ERROR: README Latest release does not match $expected_tag" >&2
  exit 1
fi

if ! grep -F "Current release tag: \`$expected_tag\`" "$readme" >/dev/null; then
  echo "ERROR: README Current release tag does not match $expected_tag" >&2
  exit 1
fi

echo "OK: release metadata matches $expected_tag"
