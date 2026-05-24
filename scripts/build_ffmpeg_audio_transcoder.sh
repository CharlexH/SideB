#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname "$0")/.." && pwd)
dockerfile="$repo_root/packaging/ffmpeg/Dockerfile"
output_path="$repo_root/package/SideB.pak/ffmpeg-lite"
metadata_dir="$repo_root/dist/ffmpeg-audio-transcoder"
image_tag="sideb/ffmpeg-audio-transcoder:local"
ffmpeg_version="${FFMPEG_VERSION:-7.1.1}"
lame_version="${LAME_VERSION:-3.100}"
target_triple="${TARGET_TRIPLE:-aarch64-linux-gnu}"
docker_no_cache=0
print_config=0

usage() {
  cat <<EOF
Usage: $0 [options]

Build SideB's trimmed bundled audio transcoder in Docker and write it to:
  $output_path

Options:
  --output <path>        Override output binary path
  --metadata-dir <path>  Override metadata output directory
  --image-tag <tag>      Override Docker image tag
  --no-cache             Pass --no-cache to docker build
  --print-config         Print pinned build configuration and exit
  -h, --help             Show this help text
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --output)
      output_path="$2"
      shift 2
      ;;
    --metadata-dir)
      metadata_dir="$2"
      shift 2
      ;;
    --image-tag)
      image_tag="$2"
      shift 2
      ;;
    --no-cache)
      docker_no_cache=1
      shift
      ;;
    --print-config)
      print_config=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [ "$print_config" -eq 1 ]; then
  cat <<EOF
dockerfile=$dockerfile
output=$output_path
metadata_dir=$metadata_dir
image_tag=$image_tag
ffmpeg_version=$ffmpeg_version
lame_version=$lame_version
target_triple=$target_triple
required_encoder=libmp3lame
required_muxer=s16le
EOF
  exit 0
fi

if [ ! -f "$dockerfile" ]; then
  echo "ERROR: missing Dockerfile at $dockerfile" >&2
  exit 1
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "ERROR: docker is required to build the bundled audio transcoder" >&2
  exit 1
fi

if ! docker info >/dev/null 2>&1; then
  echo "ERROR: docker is installed but the daemon is not reachable" >&2
  exit 1
fi

tmpdir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmpdir"
}
trap cleanup EXIT

export_dir="$tmpdir/export"

build_args=(
  build
  --platform linux/arm64
  --target artifact
  --output "type=local,dest=$export_dir"
  --build-arg "FFMPEG_VERSION=$ffmpeg_version"
  --build-arg "LAME_VERSION=$lame_version"
  --build-arg "TARGET_TRIPLE=$target_triple"
  -f "$dockerfile"
  -t "$image_tag"
  "$repo_root"
)

if [ "$docker_no_cache" -eq 1 ]; then
  build_args=(build --no-cache "${build_args[@]:1}")
fi

docker "${build_args[@]}"

mkdir -p "$metadata_dir"
cp "$export_dir/out/ffmpeg-lite" "$output_path"
cp "$export_dir/out/ffmpeg-lite.sha256" "$metadata_dir/ffmpeg-lite.sha256"
cp "$export_dir/out/ffmpeg-build-info.txt" "$metadata_dir/ffmpeg-build-info.txt"
chmod +x "$output_path"

"$repo_root/scripts/check_ffmpeg_audio_transcoder.sh" "$output_path"

echo "Built bundled audio transcoder at $output_path"
echo "Metadata written to $metadata_dir/ffmpeg-lite.sha256 and $metadata_dir/ffmpeg-build-info.txt"
