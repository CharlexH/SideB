#!/bin/sh
set -eu

progdir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
cd "$progdir" || exit 1

export SIDEB_APP_DIR="$progdir"
export SIDEB_DATA_DIR="${SIDEB_DATA_DIR:-$progdir/data}"
export SIDEB_RESOURCES_DIR="${SIDEB_RESOURCES_DIR:-$progdir/resources}"
if [ -n "${LD_LIBRARY_PATH:-}" ]; then
    export LD_LIBRARY_PATH="$LD_LIBRARY_PATH:$progdir:/usr/trimui/lib"
else
    export LD_LIBRARY_PATH="$progdir:/usr/trimui/lib"
fi

CPU_FREQ="${SIDEB_CPU_FREQ_PATH:-/sys/devices/system/cpu/cpu0/cpufreq}"
CPU_STATE_SAVED=0
PREV_GOVERNOR=
PREV_MIN_FREQ=
PREV_MAX_FREQ=
BACKEND_PID=
SIDEB_LOCK_DIR=/tmp/sideb-launch.lock
LOCK_ACQUIRED=0
SPOTIFY_AUDIO_PIPE=/tmp/sideb-spotify.pcm
export SIDEB_SPOTIFY_PIPE="$SPOTIFY_AUDIO_PIPE"

acquire_launch_lock() {
    if ! mkdir "$SIDEB_LOCK_DIR" 2>/dev/null; then
        echo "launch: SideB is already starting or running" >&2
        exit 1
    fi
    LOCK_ACQUIRED=1
}

run_spotify_backend_supervisor() {
    while true; do
        echo "launch: spotify audio_backend=pipe audio_output_pipe=$SPOTIFY_AUDIO_PIPE" >> /tmp/go-librespot.log
        status=0
        /tmp/go-librespot \
            --config_dir "$SIDEB_DATA_DIR" \
            --conf "audio_backend=pipe" \
            --conf "audio_output_pipe=$SPOTIFY_AUDIO_PIPE" \
            --conf "audio_output_pipe_format=s16le" \
            --conf "external_volume=true" \
            >> /tmp/go-librespot.log 2>&1 || status=$?
        echo "launch: go-librespot exited status=$status; restarting in 1s" >> /tmp/go-librespot.log
        sleep 1
    done
}

start_spotify_backend() {
    run_spotify_backend_supervisor &
    BACKEND_PID=$!
}

prepare_spotify_audio_pipe() {
    rm -f "$SPOTIFY_AUDIO_PIPE"
    mkfifo "$SPOTIFY_AUDIO_PIPE"
}

prepare_usb_audio_host() {
    for power_control in \
        /sys/devices/platform/soc/*ehci*-controller/power/control \
        /sys/devices/platform/soc/*ohci*-controller/power/control \
        /sys/bus/usb/devices/*/power/control \
        /sys/bus/usb/devices/usb*/power/control \
        /sys/bus/usb/devices/*-0:1.0/usb*-port*/power/control; do
        [ -w "$power_control" ] && printf '%s\n' on > "$power_control"
    done
}

save_cpu_state() {
    if [ -r "$CPU_FREQ/scaling_governor" ] &&
       [ -r "$CPU_FREQ/scaling_min_freq" ] &&
       [ -r "$CPU_FREQ/scaling_max_freq" ]; then
        PREV_GOVERNOR=$(cat "$CPU_FREQ/scaling_governor") || return 0
        PREV_MIN_FREQ=$(cat "$CPU_FREQ/scaling_min_freq") || return 0
        PREV_MAX_FREQ=$(cat "$CPU_FREQ/scaling_max_freq") || return 0
        CPU_STATE_SAVED=1
    fi
}

restore_cpu_state() {
    [ "$CPU_STATE_SAVED" = "1" ] || return 0
    [ -w "$CPU_FREQ/scaling_governor" ] && printf '%s\n' "$PREV_GOVERNOR" > "$CPU_FREQ/scaling_governor"
    [ -w "$CPU_FREQ/scaling_min_freq" ] && printf '%s\n' "$PREV_MIN_FREQ" > "$CPU_FREQ/scaling_min_freq"
    [ -w "$CPU_FREQ/scaling_max_freq" ] && printf '%s\n' "$PREV_MAX_FREQ" > "$CPU_FREQ/scaling_max_freq"
}

cleanup() {
    [ -n "$BACKEND_PID" ] && kill "$BACKEND_PID" 2>/dev/null || true
    killall go-librespot 2>/dev/null || true
    rm -f "$SPOTIFY_AUDIO_PIPE" || true
    rm -f /tmp/stay_awake || true
    rm -f /tmp/stay_alive || true
    restore_cpu_state || true
    [ "$LOCK_ACQUIRED" = "1" ] && rmdir "$SIDEB_LOCK_DIR" 2>/dev/null || true
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

acquire_launch_lock
save_cpu_state

if [ -f "$SIDEB_RESOURCES_DIR/ca-certificates.crt" ]; then
    export SSL_CERT_FILE="$SIDEB_RESOURCES_DIR/ca-certificates.crt"
elif [ -f /etc/ssl/certs/ca-certificates.crt ]; then
    export SSL_CERT_FILE="/etc/ssl/certs/ca-certificates.crt"
fi

echo 1 > /tmp/stay_awake
echo 1 > /tmp/stay_alive
prepare_usb_audio_host

copy_required_runtime() {
    cp "$progdir/go-librespot" /tmp/go-librespot
    cp "$progdir/sideb" /tmp/sideb
    chmod +x /tmp/go-librespot /tmp/sideb
}

copy_optional_runtime() {
    src=$1
    dest=$2
    [ -f "$src" ] || return 0
    cp "$src" "$dest"
    chmod +x "$dest"
}

killall go-librespot 2>/dev/null || true
killall sideb 2>/dev/null || true
sleep 1
echo 1 > /tmp/stay_awake
echo 1 > /tmp/stay_alive
prepare_usb_audio_host

copy_required_runtime

copy_optional_runtime "$progdir/yt-dlp" /tmp/yt-dlp
copy_optional_runtime "$progdir/ffmpeg-lite" /tmp/ffmpeg-lite
copy_optional_runtime "$progdir/node" /tmp/node

mkdir -p "$SIDEB_DATA_DIR"
: > /tmp/go-librespot.log
prepare_spotify_audio_pipe || exit 1
start_spotify_backend

APP_STATUS=0
/tmp/sideb 2>/tmp/sideb.log || APP_STATUS=$?
exit "$APP_STATUS"
