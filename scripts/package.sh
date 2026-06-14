#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname "$0")/.." && pwd)
target_triple="aarch64-unknown-linux-gnu"
target_glibc="2.33"
binary_path="$repo_root/spotify-ui-rs/target/$target_triple/release/sideb"
package_source="$repo_root/package/SideB.pak"
dist_root="$repo_root/dist"
stage_root="$dist_root/stage"
ffmpeg_check_script="$repo_root/scripts/check_ffmpeg_audio_transcoder.sh"
release_metadata_check_script="$repo_root/scripts/check_release_metadata.sh"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo_root/spotify-ui-rs/Cargo.toml" | head -n 1)
if [ -z "$version" ]; then
  echo "ERROR: failed to read version from spotify-ui-rs/Cargo.toml" >&2
  exit 1
fi

require_file() {
  if [ ! -f "$1" ]; then
    echo "ERROR: missing $1" >&2
    exit 1
  fi
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "ERROR: missing required command: $1" >&2
    exit 1
  fi
}

copy_resource_tree() {
  local dest="$1"
  mkdir -p "$dest"

  local resource_name
  for resource_name in \
    tapeA.png \
    play.png \
    fav.png \
    fav_on.png \
    taperoll.png \
    font_mono.ttf \
    font_mono_bak.ttf \
    tapeBase.png \
    pause.png \
    fav_off.png \
    bat0.png \
    bat25.png \
    bat50.png \
    bat75.png \
    bat100.png \
    bat_charging.png \
    wheel.png \
    cover_mask.png \
    spotify_off.png \
    icon.png \
    spotify_on.png \
    ca-certificates.crt
  do
    require_file "$package_source/resources/$resource_name"
    cp "$package_source/resources/$resource_name" "$dest/"
  done
}

build_platform_package() {
  local platform="$1"
  local rel_root="$2"
  local asset_name="SideB-${version}-${platform}.zip"
  local stage_dir="$stage_root/$platform"
  local app_root="$stage_dir/$rel_root"

  rm -rf "$stage_dir"
  mkdir -p "$app_root/resources" "$app_root/data" "$app_root/LICENSES"

  cp "$binary_path" "$app_root/sideb"
  cp "$package_source/go-librespot" "$app_root/go-librespot"
  cp "$package_source/yt-dlp" "$app_root/yt-dlp"
  cp "$package_source/ffmpeg-lite" "$app_root/ffmpeg-lite"
  chmod +x "$app_root/sideb" "$app_root/go-librespot" "$app_root/yt-dlp" "$app_root/ffmpeg-lite"

  copy_resource_tree "$app_root/resources"
  cp "$package_source/data/config.yml" "$app_root/data/config.yml"
  cp "$repo_root/packaging/shared/LICENSES/"* "$app_root/LICENSES/"
  cp "$repo_root/package/SideB.pak/icon.png" "$app_root/icon.png"

  cp "$repo_root/packaging/$platform/launch.sh" "$app_root/launch.sh"
  chmod +x "$app_root/launch.sh"
  if [ -f "$repo_root/packaging/$platform/config.json" ]; then
    cp "$repo_root/packaging/$platform/config.json" "$app_root/config.json"
  fi

  rm -f "$dist_root/$asset_name"
  (
    cd "$stage_dir"
    zip -qr "$dist_root/$asset_name" .
  )

  echo "Built: $dist_root/$asset_name"
}

zip_has_entry() {
  local zip_path="$1"
  local entry="$2"
  zipinfo -1 "$zip_path" | grep -Fx -- "$entry" >/dev/null
}

zip_has_prefix() {
  local zip_path="$1"
  local prefix="$2"
  zipinfo -1 "$zip_path" | awk -v prefix="$prefix" 'index($0, prefix) == 1 { found = 1 } END { exit found ? 0 : 1 }'
}

assert_zip_entry() {
  local zip_path="$1"
  local entry="$2"
  if ! zip_has_entry "$zip_path" "$entry"; then
    echo "ERROR: $zip_path is missing zip entry $entry" >&2
    exit 1
  fi
}

assert_zip_no_entry() {
  local zip_path="$1"
  local entry="$2"
  if zip_has_entry "$zip_path" "$entry"; then
    echo "ERROR: $zip_path unexpectedly contains zip entry $entry" >&2
    exit 1
  fi
}

assert_zip_no_prefix() {
  local zip_path="$1"
  local prefix="$2"
  if zip_has_prefix "$zip_path" "$prefix"; then
    echo "ERROR: $zip_path unexpectedly contains entries under $prefix" >&2
    exit 1
  fi
}

validate_zip_layout() {
  local platform="$1"
  local root="$2"
  local zip_path="$dist_root/SideB-${version}-${platform}.zip"

  require_file "$zip_path"

  assert_zip_entry "$zip_path" "${root}launch.sh"
  assert_zip_entry "$zip_path" "${root}sideb"
  assert_zip_entry "$zip_path" "${root}go-librespot"
  assert_zip_entry "$zip_path" "${root}yt-dlp"
  assert_zip_entry "$zip_path" "${root}ffmpeg-lite"
  assert_zip_entry "$zip_path" "${root}data/config.yml"
  assert_zip_entry "$zip_path" "${root}resources/tapeBase.png"
  assert_zip_entry "$zip_path" "${root}resources/font_mono.ttf"
  assert_zip_entry "$zip_path" "${root}resources/bat0.png"
  assert_zip_entry "$zip_path" "${root}resources/bat25.png"
  assert_zip_entry "$zip_path" "${root}resources/bat50.png"
  assert_zip_entry "$zip_path" "${root}resources/bat75.png"
  assert_zip_entry "$zip_path" "${root}resources/bat100.png"
  assert_zip_entry "$zip_path" "${root}resources/bat_charging.png"
  assert_zip_entry "$zip_path" "${root}resources/ca-certificates.crt"
  assert_zip_entry "$zip_path" "${root}icon.png"
  assert_zip_entry "$zip_path" "${root}LICENSES/NOTICE.md"
  assert_zip_entry "$zip_path" "${root}LICENSES/THIRD_PARTY_SOURCES.md"
  assert_zip_no_entry "$zip_path" "${root}resources/Fullscreen - Mesh Grid.png"
}

validate_release_packages() {
  local expected_asset_count
  local pak_release_filename
  local stable_asset
  pak_release_filename=$(sed -n 's/^[[:space:]]*"release_filename":[[:space:]]*"\([^"]*\)".*/\1/p' "$repo_root/pak.json" | head -n 1)

  if [ "$pak_release_filename" != "SideB-${version}-nextui.zip" ]; then
    echo "ERROR: pak.json release_filename ($pak_release_filename) does not match SideB-${version}-nextui.zip" >&2
    exit 1
  fi

  require_file "$dist_root/$pak_release_filename"

  validate_zip_layout "nextui" ""
  assert_zip_no_prefix "$dist_root/SideB-${version}-nextui.zip" "Tools/"
  assert_zip_no_prefix "$dist_root/SideB-${version}-nextui.zip" "Apps/"

  validate_zip_layout "stock" "Apps/SideB/"
  assert_zip_no_prefix "$dist_root/SideB-${version}-stock.zip" "Tools/"
  assert_zip_no_entry "$dist_root/SideB-${version}-stock.zip" "launch.sh"
  assert_zip_entry "$dist_root/SideB-${version}-stock.zip" "Apps/SideB/config.json"

  validate_zip_layout "crossmix" "Apps/SideB/"
  assert_zip_no_prefix "$dist_root/SideB-${version}-crossmix.zip" "Tools/"
  assert_zip_no_entry "$dist_root/SideB-${version}-crossmix.zip" "launch.sh"
  assert_zip_entry "$dist_root/SideB-${version}-crossmix.zip" "Apps/SideB/config.json"

  expected_asset_count=0
  for stable_asset in \
    "SideB-${version}-nextui.zip" \
    "SideB-${version}-stock.zip" \
    "SideB-${version}-crossmix.zip"
  do
    [ -f "$dist_root/$stable_asset" ] && expected_asset_count=$((expected_asset_count + 1))
  done
  if [ "$expected_asset_count" != "3" ]; then
    echo "ERROR: expected exactly 3 release zips for $version, found $expected_asset_count" >&2
    find "$dist_root" -maxdepth 1 -type f \
      \( -name "SideB-${version}-nextui.zip" \
      -o -name "SideB-${version}-stock.zip" \
      -o -name "SideB-${version}-crossmix.zip" \) -print >&2
    exit 1
  fi

  echo "OK: release zip layouts match pak store and manual install expectations"
}

require_command zip
require_command zipinfo
require_command zig
require_command cargo-zigbuild
require_file "$package_source/go-librespot"
require_file "$package_source/yt-dlp"
require_file "$package_source/ffmpeg-lite"
require_file "$ffmpeg_check_script"
require_file "$release_metadata_check_script"
require_file "$package_source/data/config.yml"
require_file "$repo_root/packaging/shared/LICENSES/NOTICE.md"
require_file "$repo_root/packaging/shared/LICENSES/THIRD_PARTY_SOURCES.md"

"$ffmpeg_check_script" "$package_source/ffmpeg-lite"
"$release_metadata_check_script" "$repo_root"

mkdir -p "$dist_root"

echo "Building sideb $version for $target_triple glibc $target_glibc"
(
  cd "$repo_root/spotify-ui-rs"
  cargo zigbuild --release --target "$target_triple.$target_glibc"
)
require_file "$binary_path"

# Pak Store extracts TOOL zip assets into /mnt/SDCARD/Tools/<platform>/<name>.pak.
# Keep the NextUI archive rooted at the pak contents to avoid a nested
# Tools/tg5040/SideB.pak/Tools/tg5040/SideB.pak install.
build_platform_package "nextui" "."
build_platform_package "stock" "Apps/SideB"
build_platform_package "crossmix" "Apps/SideB"

validate_release_packages

echo "Done. Release zips are in $dist_root"
