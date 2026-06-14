#!/bin/sh

# HELP: Experimental SideB Candidate package for device validation.
# ICON: sideb
# GRID: SideB

set -eu

if [ -f /opt/muos/script/var/func.sh ]; then
    . /opt/muos/script/var/func.sh
    APP_BIN="sideb"
    if command -v SETUP_APP >/dev/null 2>&1; then
        SETUP_APP "$APP_BIN" ""
    fi
    if command -v SETUP_STAGE_OVERLAY >/dev/null 2>&1; then
        SETUP_STAGE_OVERLAY
    fi
fi

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
app_name=$(basename "$script_dir")
runtime_app_dir="/run/muos/storage/application/$app_name"
if [ -d "$runtime_app_dir" ]; then
    app_dir="$runtime_app_dir"
else
    app_dir="$script_dir"
fi

if [ ! -d "$app_dir" ]; then
    echo "SideB application directory is missing: $app_dir" >&2
    exit 1
fi

cd "$app_dir" || exit 1

export SIDEB_APP_DIR="$app_dir"
export SIDEB_DATA_DIR="${SIDEB_DATA_DIR:-$app_dir/data}"
export SIDEB_RESOURCES_DIR="${SIDEB_RESOURCES_DIR:-$app_dir/resources}"

mkdir -p "$SIDEB_DATA_DIR/home" "$SIDEB_DATA_DIR/config"
export HOME="${HOME:-$SIDEB_DATA_DIR/home}"
export XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-$SIDEB_DATA_DIR/config}"

append_ld_library_path() {
    if [ -n "${LD_LIBRARY_PATH:-}" ]; then
        LD_LIBRARY_PATH="$LD_LIBRARY_PATH:$1"
    else
        LD_LIBRARY_PATH="$1"
    fi
}

append_ld_library_path "$app_dir"
[ -d /usr/lib ] && append_ld_library_path /usr/lib
[ -d /usr/local/lib ] && append_ld_library_path /usr/local/lib
export LD_LIBRARY_PATH

BACKEND_PID=

cleanup() {
    [ -n "$BACKEND_PID" ] && kill "$BACKEND_PID" 2>/dev/null || true
    killall go-librespot 2>/dev/null || true
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [ -f "$SIDEB_RESOURCES_DIR/ca-certificates.crt" ]; then
    export SSL_CERT_FILE="$SIDEB_RESOURCES_DIR/ca-certificates.crt"
elif [ -f /etc/ssl/certs/ca-certificates.crt ]; then
    export SSL_CERT_FILE="/etc/ssl/certs/ca-certificates.crt"
fi

killall go-librespot 2>/dev/null || true
killall sideb 2>/dev/null || true
sleep 1

cp "$app_dir/go-librespot" /tmp/go-librespot
cp "$app_dir/sideb" /tmp/sideb
chmod +x /tmp/go-librespot /tmp/sideb

[ -f "$app_dir/yt-dlp" ] && cp "$app_dir/yt-dlp" /tmp/yt-dlp && chmod +x /tmp/yt-dlp
[ -f "$app_dir/ffmpeg-lite" ] && cp "$app_dir/ffmpeg-lite" /tmp/ffmpeg-lite && chmod +x /tmp/ffmpeg-lite
[ -f "$app_dir/node" ] && cp "$app_dir/node" /tmp/node && chmod +x /tmp/node

mkdir -p "$SIDEB_DATA_DIR"
/tmp/go-librespot --config_dir "$SIDEB_DATA_DIR" > /tmp/go-librespot.log 2>&1 &
BACKEND_PID=$!

APP_STATUS=0
/tmp/sideb 2>/tmp/sideb.log || APP_STATUS=$?
exit "$APP_STATUS"
