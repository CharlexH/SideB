#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname "$0")/.." && pwd)
check_script="$repo_root/scripts/check_release_metadata.sh"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

write_fixture() {
  local root="$1"
  local cargo_version="$2"
  local pak_version="$3"
  local release_filename="$4"
  local changelog_version="$5"
  local readme_latest="$6"
  local readme_current="$7"

  mkdir -p "$root/spotify-ui-rs"

  cat >"$root/spotify-ui-rs/Cargo.toml" <<EOF
[package]
name = "sideb"
version = "$cargo_version"
edition = "2021"
EOF

  cat >"$root/pak.json" <<EOF
{
  "name": "SideB",
  "version": "$pak_version",
  "release_filename": "$release_filename",
  "changelog": {
    "$changelog_version": "Fixture release notes"
  }
}
EOF

  cat >"$root/README.md" <<EOF
# SideB

Latest release: \`$readme_latest\`

Current release tag: \`$readme_current\`
EOF
}

valid="$tmpdir/valid"
write_fixture "$valid" "1.1.0" "v1.1.0" "SideB-1.1.0-nextui.zip" "v1.1.0" "v1.1.0" "v1.1.0"
"$check_script" "$valid" >/dev/null

bad_pak_version="$tmpdir/bad-pak-version"
write_fixture "$bad_pak_version" "1.1.0" "v1.0.12" "SideB-1.1.0-nextui.zip" "v1.1.0" "v1.1.0" "v1.1.0"
if "$check_script" "$bad_pak_version" >/dev/null 2>&1; then
  echo "ERROR: metadata checker accepted a stale pak.json version" >&2
  exit 1
fi

bad_filename="$tmpdir/bad-filename"
write_fixture "$bad_filename" "1.1.0" "v1.1.0" "SideB-1.0.12-nextui.zip" "v1.1.0" "v1.1.0" "v1.1.0"
if "$check_script" "$bad_filename" >/dev/null 2>&1; then
  echo "ERROR: metadata checker accepted a stale release_filename" >&2
  exit 1
fi

missing_changelog="$tmpdir/missing-changelog"
write_fixture "$missing_changelog" "1.1.0" "v1.1.0" "SideB-1.1.0-nextui.zip" "v1.0.12" "v1.1.0" "v1.1.0"
if "$check_script" "$missing_changelog" >/dev/null 2>&1; then
  echo "ERROR: metadata checker accepted a missing changelog entry" >&2
  exit 1
fi

stale_readme="$tmpdir/stale-readme"
write_fixture "$stale_readme" "1.1.0" "v1.1.0" "SideB-1.1.0-nextui.zip" "v1.1.0" "v1.0.12" "v1.1.0"
if "$check_script" "$stale_readme" >/dev/null 2>&1; then
  echo "ERROR: metadata checker accepted a stale README latest release" >&2
  exit 1
fi

echo "OK: release metadata checker enforces pak.json and README version sync"
