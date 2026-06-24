#!/usr/bin/env bash
set -euo pipefail

tag="${1:-}"
repo="${GH_REPO:-CharlexH/SideB}"
repo_root=$(cd -- "$(dirname "$0")/.." && pwd)
dist_dir="${SIDEB_DIST_DIR:-$repo_root/dist}"

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
assets_tsv="$tmpdir/assets.tsv"

printf '%s\n' \
  "SideB-${version}-crossmix.zip" \
  "SideB-${version}-nextui.zip" \
  "SideB-${version}-stock.zip" \
  | sort >"$expected"

gh api "repos/${repo}/releases/tags/${tag}" \
  --jq '.assets[] | [.name, (.digest // "")] | @tsv' \
  | sort >"$assets_tsv"

cut -f1 "$assets_tsv" | sort >"$actual"

grep -E "^SideB-${version}-(crossmix|nextui|stock)\\.zip$" "$actual" >"$actual_stable" || true

if ! diff -u "$expected" "$actual_stable"; then
  echo "ERROR: GitHub release $tag assets are missing one or more stable SideB $version release zips" >&2
  exit 1
fi

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

asset_digest() {
  local asset_name=$1
  awk -v name="$asset_name" -F '\t' '$1 == name { print $2; found = 1 } END { if (!found) exit 1 }' "$assets_tsv"
}

while IFS= read -r asset_name; do
  local_path="$dist_dir/$asset_name"
  if [ ! -f "$local_path" ]; then
    continue
  fi

  remote_digest=$(asset_digest "$asset_name")
  if [ -z "$remote_digest" ]; then
    echo "ERROR: GitHub release $tag asset $asset_name has no digest in API response" >&2
    exit 1
  fi

  local_digest="sha256:$(sha256_file "$local_path")"
  if [ "$remote_digest" != "$local_digest" ]; then
    echo "ERROR: GitHub release $tag asset $asset_name digest $remote_digest does not match local $local_digest" >&2
    exit 1
  fi
done <"$expected"

echo "OK: GitHub release $tag has the expected stable SideB $version assets and matching local digests when present"
