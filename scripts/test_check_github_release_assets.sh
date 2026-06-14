#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname "$0")/.." && pwd)
check_script="$repo_root/scripts/check_github_release_assets.sh"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

fake_bin="$tmpdir/bin"
mkdir -p "$fake_bin"

cat >"$fake_bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [ "${FAKE_GH_MISSING_CROSSMIX:-0}" = "1" ]; then
  printf '%s\n' \
    SideB-1.1.0-nextui.zip \
    SideB-1.1.0-stock.zip
else
  printf '%s\n' \
    SideB-1.1.0-crossmix.zip \
    SideB-1.1.0-knulli-candidate.zip \
    SideB-1.1.0-muos-candidate.muxapp \
    SideB-1.1.0-nextui.zip \
    SideB-1.1.0-stock.zip
fi
EOF
chmod +x "$fake_bin/gh"

PATH="$fake_bin:$PATH" "$check_script" v1.1.0 >/dev/null

if FAKE_GH_MISSING_CROSSMIX=1 PATH="$fake_bin:$PATH" "$check_script" v1.1.0 >/dev/null 2>&1; then
  echo "ERROR: GitHub asset checker accepted a release missing crossmix zip" >&2
  exit 1
fi

echo "OK: GitHub asset checker enforces the three stable release zip names while allowing Candidate assets"
