#!/bin/sh
progdir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
cd "$progdir" || exit 1

export SIDEB_APP_DIR="$progdir"
export SIDEB_DATA_DIR="${SIDEB_DATA_DIR:-$progdir/data}"
export SIDEB_RESOURCES_DIR="${SIDEB_RESOURCES_DIR:-$progdir/resources}"
export LD_LIBRARY_PATH="$LD_LIBRARY_PATH:$progdir:/usr/trimui/lib"

CPU_FREQ="${SIDEB_CPU_FREQ_PATH:-/sys/devices/system/cpu/cpu0/cpufreq}"
CPU_STATE_SAVED=0
PREV_GOVERNOR=
PREV_MIN_FREQ=
PREV_MAX_FREQ=
BACKEND_PID=

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
    [ -n "$BACKEND_PID" ] && kill "$BACKEND_PID" 2>/dev/null
    killall go-librespot 2>/dev/null
    rm -f /tmp/stay_awake
    rm -f /tmp/stay_alive
    restore_cpu_state
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

save_cpu_state

if [ -f "$SIDEB_RESOURCES_DIR/ca-certificates.crt" ]; then
    export SSL_CERT_FILE="$SIDEB_RESOURCES_DIR/ca-certificates.crt"
elif [ -f /etc/ssl/certs/ca-certificates.crt ]; then
    export SSL_CERT_FILE="/etc/ssl/certs/ca-certificates.crt"
fi

echo 1 > /tmp/stay_awake
echo 1 > /tmp/stay_alive

killall go-librespot 2>/dev/null
killall sideb 2>/dev/null
sleep 1
echo 1 > /tmp/stay_awake
echo 1 > /tmp/stay_alive

cp "$progdir/go-librespot" /tmp/go-librespot
cp "$progdir/sideb" /tmp/sideb
chmod +x /tmp/go-librespot /tmp/sideb

[ -f "$progdir/yt-dlp" ] && cp "$progdir/yt-dlp" /tmp/yt-dlp && chmod +x /tmp/yt-dlp
[ -f "$progdir/ffmpeg-lite" ] && cp "$progdir/ffmpeg-lite" /tmp/ffmpeg-lite && chmod +x /tmp/ffmpeg-lite
[ -f "$progdir/node" ] && cp "$progdir/node" /tmp/node && chmod +x /tmp/node

mkdir -p "$SIDEB_DATA_DIR"
/tmp/go-librespot --config_dir "$SIDEB_DATA_DIR" > /tmp/go-librespot.log 2>&1 &
BACKEND_PID=$!

APP_STATUS=0
/tmp/sideb 2>/tmp/sideb.log || APP_STATUS=$?
exit "$APP_STATUS"
