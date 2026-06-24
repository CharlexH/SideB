#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname "$0")/.." && pwd)
check_script="$repo_root/scripts/check_github_release_assets.sh"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

fake_bin="$tmpdir/bin"
dist_dir="$tmpdir/dist"
mkdir -p "$fake_bin"
mkdir -p "$dist_dir"

printf 'crossmix\n' >"$dist_dir/SideB-1.1.0-crossmix.zip"
printf 'nextui\n' >"$dist_dir/SideB-1.1.0-nextui.zip"
printf 'stock\n' >"$dist_dir/SideB-1.1.0-stock.zip"
crossmix_sha=$(shasum -a 256 "$dist_dir/SideB-1.1.0-crossmix.zip" | awk '{print $1}')
nextui_sha=$(shasum -a 256 "$dist_dir/SideB-1.1.0-nextui.zip" | awk '{print $1}')
stock_sha=$(shasum -a 256 "$dist_dir/SideB-1.1.0-stock.zip" | awk '{print $1}')

cat >"$fake_bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [ "${FAKE_GH_MISSING_CROSSMIX:-0}" = "1" ]; then
  printf '%s\n' \
    "SideB-1.1.0-nextui.zip	sha256:${NEXTUI_SHA}" \
    "SideB-1.1.0-stock.zip	sha256:${STOCK_SHA}"
else
  crossmix_digest="sha256:${CROSSMIX_SHA}"
  if [ "${FAKE_GH_BAD_CROSSMIX_DIGEST:-0}" = "1" ]; then
    crossmix_digest="sha256:0000000000000000000000000000000000000000000000000000000000000000"
  fi
  printf '%s\n' \
    "SideB-1.1.0-crossmix.zip	${crossmix_digest}" \
    "SideB-1.1.0-knulli-candidate.zip	sha256:candidate" \
    "SideB-1.1.0-muos-candidate.muxapp	sha256:candidate" \
    "SideB-1.1.0-nextui.zip	sha256:${NEXTUI_SHA}" \
    "SideB-1.1.0-stock.zip	sha256:${STOCK_SHA}"
fi
EOF
chmod +x "$fake_bin/gh"

CROSSMIX_SHA="$crossmix_sha" NEXTUI_SHA="$nextui_sha" STOCK_SHA="$stock_sha" SIDEB_DIST_DIR="$dist_dir" PATH="$fake_bin:$PATH" "$check_script" v1.1.0 >/dev/null

if FAKE_GH_MISSING_CROSSMIX=1 CROSSMIX_SHA="$crossmix_sha" NEXTUI_SHA="$nextui_sha" STOCK_SHA="$stock_sha" SIDEB_DIST_DIR="$dist_dir" PATH="$fake_bin:$PATH" "$check_script" v1.1.0 >/dev/null 2>&1; then
  echo "ERROR: GitHub asset checker accepted a release missing crossmix zip" >&2
  exit 1
fi

if FAKE_GH_BAD_CROSSMIX_DIGEST=1 CROSSMIX_SHA="$crossmix_sha" NEXTUI_SHA="$nextui_sha" STOCK_SHA="$stock_sha" SIDEB_DIST_DIR="$dist_dir" PATH="$fake_bin:$PATH" "$check_script" v1.1.0 >/dev/null 2>&1; then
  echo "ERROR: GitHub asset checker accepted a release asset with the wrong SHA256 digest" >&2
  exit 1
fi

echo "OK: GitHub asset checker enforces stable zip names, allows Candidate assets, and verifies release digests"
