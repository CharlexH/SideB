#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname "$0")/.." && pwd)
target_triple="aarch64-unknown-linux-gnu"
target_glibc="2.33"
binary_path="$repo_root/spotify-ui-rs/target/$target_triple/release/sideb"
package_source="$repo_root/package/SideB.pak"
dist_root="$repo_root/dist"
stage_root="$dist_root/candidate-stage"
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

copy_payload_tree() {
  local app_root="$1"
  mkdir -p "$app_root/resources" "$app_root/data" "$app_root/LICENSES"

  cp "$binary_path" "$app_root/sideb"
  cp "$package_source/go-librespot" "$app_root/go-librespot"
  cp "$package_source/yt-dlp" "$app_root/yt-dlp"
  cp "$package_source/ffmpeg-lite" "$app_root/ffmpeg-lite"
  chmod +x "$app_root/sideb" "$app_root/go-librespot" "$app_root/yt-dlp" "$app_root/ffmpeg-lite"

  copy_resource_tree "$app_root/resources"
  cp "$package_source/data/config.yml" "$app_root/data/config.yml"
  cp "$repo_root/packaging/shared/LICENSES/"* "$app_root/LICENSES/"
  cp "$package_source/icon.png" "$app_root/icon.png"
}

zip_has_entry() {
  local zip_path="$1"
  local entry="$2"
  zipinfo -1 "$zip_path" | grep -Fx -- "$entry" >/dev/null
}

assert_zip_entry() {
  local zip_path="$1"
  local entry="$2"
  if ! zip_has_entry "$zip_path" "$entry"; then
    echo "ERROR: $zip_path is missing zip entry $entry" >&2
    exit 1
  fi
}

build_knulli_candidate() {
  local asset_name="SideB-${version}-knulli-candidate.zip"
  local stage_dir="$stage_root/knulli"
  local port_root="$stage_dir/sideb"
  local app_root="$port_root/sideb"

  rm -rf "$stage_dir"
  mkdir -p "$port_root"
  copy_payload_tree "$app_root"

  cp "$repo_root/packaging/knulli/SideB.sh" "$port_root/SideB.sh"
  sed "s/__VERSION__/$version/g" "$repo_root/packaging/knulli/port.json" > "$port_root/port.json"
  cp "$repo_root/packaging/knulli/gameinfo.xml" "$port_root/gameinfo.xml"
  cp "$repo_root/packaging/knulli/README.md" "$port_root/README.md"
  cp "$repo_root/screenshots/offline_playing.png" "$port_root/screenshot.png"
  chmod +x "$port_root/SideB.sh"

  rm -f "$dist_root/$asset_name"
  (
    cd "$stage_dir"
    zip -qr "$dist_root/$asset_name" .
  )

  echo "Built: $dist_root/$asset_name"
}

build_muos_candidate() {
  local asset_name="SideB-${version}-muos-candidate.muxapp"
  local stage_dir="$stage_root/muos"
  local app_root="$stage_dir/SideB"

  rm -rf "$stage_dir"
  mkdir -p "$app_root/glyph"
  copy_payload_tree "$app_root"

  cp "$repo_root/packaging/muos/mux_launch.sh" "$app_root/mux_launch.sh"
  cp "$repo_root/packaging/muos/mux_launch.ini" "$app_root/mux_launch.ini"
  cp "$repo_root/packaging/muos/mux_lang.ini" "$app_root/mux_lang.ini"
  cp "$package_source/icon.png" "$app_root/glyph/sideb.png"
  chmod +x "$app_root/mux_launch.sh"

  rm -f "$dist_root/$asset_name"
  (
    cd "$stage_dir"
    zip -qr "$dist_root/$asset_name" .
  )

  echo "Built: $dist_root/$asset_name"
}

validate_candidate_packages() {
  local knulli_zip="$dist_root/SideB-${version}-knulli-candidate.zip"
  local muos_zip="$dist_root/SideB-${version}-muos-candidate.muxapp"

  require_file "$knulli_zip"
  assert_zip_entry "$knulli_zip" "sideb/SideB.sh"
  assert_zip_entry "$knulli_zip" "sideb/port.json"
  assert_zip_entry "$knulli_zip" "sideb/gameinfo.xml"
  assert_zip_entry "$knulli_zip" "sideb/README.md"
  assert_zip_entry "$knulli_zip" "sideb/screenshot.png"
  assert_zip_entry "$knulli_zip" "sideb/sideb/sideb"
  assert_zip_entry "$knulli_zip" "sideb/sideb/go-librespot"
  assert_zip_entry "$knulli_zip" "sideb/sideb/yt-dlp"
  assert_zip_entry "$knulli_zip" "sideb/sideb/ffmpeg-lite"
  assert_zip_entry "$knulli_zip" "sideb/sideb/data/config.yml"
  assert_zip_entry "$knulli_zip" "sideb/sideb/resources/tapeBase.png"
  assert_zip_entry "$knulli_zip" "sideb/sideb/resources/ca-certificates.crt"
  assert_zip_entry "$knulli_zip" "sideb/sideb/LICENSES/NOTICE.md"

  require_file "$muos_zip"
  assert_zip_entry "$muos_zip" "SideB/mux_launch.sh"
  assert_zip_entry "$muos_zip" "SideB/mux_launch.ini"
  assert_zip_entry "$muos_zip" "SideB/mux_lang.ini"
  assert_zip_entry "$muos_zip" "SideB/glyph/sideb.png"
  assert_zip_entry "$muos_zip" "SideB/sideb"
  assert_zip_entry "$muos_zip" "SideB/go-librespot"
  assert_zip_entry "$muos_zip" "SideB/yt-dlp"
  assert_zip_entry "$muos_zip" "SideB/ffmpeg-lite"
  assert_zip_entry "$muos_zip" "SideB/data/config.yml"
  assert_zip_entry "$muos_zip" "SideB/resources/tapeBase.png"
  assert_zip_entry "$muos_zip" "SideB/resources/ca-certificates.crt"
  assert_zip_entry "$muos_zip" "SideB/LICENSES/NOTICE.md"

  echo "OK: Candidate package layouts match KNULLI and muOS expectations"
}

require_command zip
require_command zipinfo
require_file "$package_source/go-librespot"
require_file "$package_source/yt-dlp"
require_file "$package_source/ffmpeg-lite"
require_file "$ffmpeg_check_script"
require_file "$release_metadata_check_script"
require_file "$package_source/data/config.yml"
require_file "$package_source/icon.png"
require_file "$repo_root/screenshots/offline_playing.png"
require_file "$repo_root/packaging/shared/LICENSES/NOTICE.md"
require_file "$repo_root/packaging/shared/LICENSES/THIRD_PARTY_SOURCES.md"
require_file "$repo_root/packaging/knulli/SideB.sh"
require_file "$repo_root/packaging/knulli/port.json"
require_file "$repo_root/packaging/knulli/gameinfo.xml"
require_file "$repo_root/packaging/knulli/README.md"
require_file "$repo_root/packaging/muos/mux_launch.sh"
require_file "$repo_root/packaging/muos/mux_launch.ini"
require_file "$repo_root/packaging/muos/mux_lang.ini"

"$ffmpeg_check_script" "$package_source/ffmpeg-lite"
"$release_metadata_check_script" "$repo_root"

if [ "${SIDEB_SKIP_CANDIDATE_BUILD:-0}" != "1" ]; then
  require_command zig
  require_command cargo-zigbuild
  echo "Building sideb $version for $target_triple glibc $target_glibc"
  (
    cd "$repo_root/spotify-ui-rs"
    cargo zigbuild --release --target "$target_triple.$target_glibc"
  )
fi
require_file "$binary_path"

mkdir -p "$dist_root"
build_knulli_candidate
build_muos_candidate
validate_candidate_packages

echo "Done. Candidate release packages are in $dist_root"
