#!/usr/bin/env bash
set -euo pipefail

tag="${1:-}"
repo="${GH_REPO:-CharlexH/SideB}"

if [ -z "$tag" ]; then
  echo "Usage: $0 vX.Y.Z" >&2
  exit 2
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "ERROR: missing required command: gh" >&2
  exit 1
fi

version="${tag#v}"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

expected="$tmpdir/expected"
actual="$tmpdir/actual"
actual_stable="$tmpdir/actual-stable"

printf '%s\n' \
  "SideB-${version}-crossmix.zip" \
  "SideB-${version}-nextui.zip" \
  "SideB-${version}-stock.zip" \
  | sort >"$expected"

gh release view "$tag" \
  --repo "$repo" \
  --json assets \
  --jq '.assets[].name' \
  | sort >"$actual"

grep -E "^SideB-${version}-(crossmix|nextui|stock)\\.zip$" "$actual" >"$actual_stable" || true

if ! diff -u "$expected" "$actual_stable"; then
  echo "ERROR: GitHub release $tag assets are missing one or more stable SideB $version release zips" >&2
  exit 1
fi

echo "OK: GitHub release $tag has the expected stable SideB $version assets"
