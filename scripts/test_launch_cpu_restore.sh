#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname "$0")/.." && pwd)

launchers=(
  "$repo_root/package/SideB.pak/launch.sh"
  "$repo_root/packaging/nextui/launch.sh"
  "$repo_root/packaging/stock/launch.sh"
  "$repo_root/packaging/crossmix/launch.sh"
)

for launcher in "${launchers[@]}"; do
  if [ ! -f "$launcher" ]; then
    echo "ERROR: missing launcher $launcher" >&2
    exit 1
  fi

  grep -F 'CPU_FREQ="${SIDEB_CPU_FREQ_PATH:-/sys/devices/system/cpu/cpu0/cpufreq}"' "$launcher" >/dev/null || {
    echo "ERROR: $launcher does not use the guarded CPU frequency path" >&2
    exit 1
  }

  grep -F 'save_cpu_state' "$launcher" >/dev/null || {
    echo "ERROR: $launcher does not save CPU state" >&2
    exit 1
  }

  grep -F 'restore_cpu_state' "$launcher" >/dev/null || {
    echo "ERROR: $launcher does not restore CPU state" >&2
    exit 1
  }

  grep -F 'trap cleanup EXIT' "$launcher" >/dev/null || {
    echo "ERROR: $launcher does not run cleanup on exit" >&2
    exit 1
  }

  grep -F "trap 'exit 130' INT" "$launcher" >/dev/null || {
    echo "ERROR: $launcher does not route INT through cleanup" >&2
    exit 1
  }

  grep -F "trap 'exit 143' TERM" "$launcher" >/dev/null || {
    echo "ERROR: $launcher does not route TERM through cleanup" >&2
    exit 1
  }
done

echo "OK: launchers preserve and restore CPU scaling state"
