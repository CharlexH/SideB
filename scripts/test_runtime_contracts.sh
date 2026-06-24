#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)

stable_launchers=(
  "$repo_root/package/SideB.pak/launch.sh"
  "$repo_root/packaging/nextui/launch.sh"
  "$repo_root/packaging/stock/launch.sh"
  "$repo_root/packaging/crossmix/launch.sh"
)

candidate_launchers=(
  "$repo_root/packaging/knulli/SideB.sh"
  "$repo_root/packaging/muos/mux_launch.sh"
)

all_launchers=("${stable_launchers[@]}" "${candidate_launchers[@]}")

require_line() {
  local file=$1
  local needle=$2
  local message=$3

  if ! grep -F -- "$needle" "$file" >/dev/null; then
    echo "ERROR: $file $message" >&2
    exit 1
  fi
}

for launcher in "${stable_launchers[@]}"; do
  require_line "$launcher" "set -eu" "must fail closed on copy/chmod/mkdir failures"
  require_line "$launcher" "SIDEB_LOCK_DIR=/tmp/sideb-launch.lock" "must use the shared launch lock"
  require_line "$launcher" "acquire_launch_lock" "must acquire the launch lock before runtime setup"
  require_line "$launcher" "copy_required_runtime" "must copy required runtime files through a fail-closed helper"
  require_line "$launcher" "copy_optional_runtime" "must copy optional helpers through a fail-closed helper"
  require_line "$launcher" "killall go-librespot 2>/dev/null || true" "must tolerate absent backend process under set -e"
  require_line "$launcher" "killall sideb 2>/dev/null || true" "must tolerate absent app process under set -e"
done

for launcher in "${all_launchers[@]}"; do
  require_line "$launcher" "SPOTIFY_AUDIO_PIPE=/tmp/sideb-spotify.pcm" "must use the shared Spotify PCM pipe"
  require_line "$launcher" "--conf \"audio_backend=pipe\"" "must start go-librespot with the pipe backend"
  require_line "$launcher" "--conf \"audio_output_pipe=\$SPOTIFY_AUDIO_PIPE\"" "must point go-librespot at the SideB PCM pipe"
  require_line "$launcher" "--conf \"audio_output_pipe_format=s16le\"" "must keep the PCM format expected by SideB"
  require_line "$launcher" "--conf \"external_volume=true\"" "must keep SideB in control of volume"
  require_line "$launcher" "prepare_spotify_audio_pipe" "must create the Spotify PCM FIFO before backend launch"
  require_line "$launcher" "rm -f \"\$SPOTIFY_AUDIO_PIPE\"" "must remove the Spotify PCM FIFO on cleanup"
  require_line "$launcher" "run_spotify_backend_supervisor" "must supervise go-librespot and restart it after unexpected exits"
  require_line "$launcher" "launch: go-librespot exited status=" "must log backend exits before restarting"
done

require_line "$repo_root/package/SideB.pak/data/config.yml" "address: \"127.0.0.1\"" "must bind the go-librespot API to loopback only"

echo "OK: runtime launch and API binding contracts hold"
