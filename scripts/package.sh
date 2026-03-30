#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname "$0")/.." && pwd)
target_triple="aarch64-unknown-linux-musl"
binary_path="$repo_root/spotify-ui-rs/target/$target_triple/release/sideb"
package_source="$repo_root/package/SideB.pak"
dist_root="$repo_root/dist"
stage_root="$dist_root/stage"
ffmpeg_check_script="$repo_root/scripts/check_ffmpeg_audio_transcoder.sh"

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

copy_resource_tree() {
  local dest="$1"
  mkdir -p "$dest"

  local resource
  for resource in "$package_source"/resources/*; do
    [ -f "$resource" ] || continue
    case "${resource##*/}" in
      *.png|*.ttf|*.crt)
        cp "$resource" "$dest/"
        ;;
    esac
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
  cp "$package_source/ffmpeg-full" "$app_root/ffmpeg-full"
  chmod +x "$app_root/sideb" "$app_root/go-librespot" "$app_root/yt-dlp" "$app_root/ffmpeg-full"

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

require_file "$package_source/go-librespot"
require_file "$package_source/yt-dlp"
require_file "$package_source/ffmpeg-full"
require_file "$ffmpeg_check_script"
require_file "$package_source/data/config.yml"
require_file "$repo_root/packaging/shared/LICENSES/NOTICE.md"
require_file "$repo_root/packaging/shared/LICENSES/THIRD_PARTY_SOURCES.md"

"$ffmpeg_check_script" "$package_source/ffmpeg-full"

mkdir -p "$dist_root"

echo "Building sideb $version for $target_triple"
(
  cd "$repo_root/spotify-ui-rs"
  cargo build --release --target "$target_triple"
)
require_file "$binary_path"

build_platform_package "nextui" "Tools/tg5040/SideB.pak"
build_platform_package "stock" "Apps/SideB"
build_platform_package "crossmix" "Apps/SideB"

echo "Done. Release zips are in $dist_root"
