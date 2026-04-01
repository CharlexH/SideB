#!/bin/sh
progdir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
cd "$progdir" || exit 1

export SIDEB_APP_DIR="$progdir"
export SIDEB_DATA_DIR="${SIDEB_DATA_DIR:-$progdir/data}"
export SIDEB_RESOURCES_DIR="${SIDEB_RESOURCES_DIR:-$progdir/resources}"

export LD_LIBRARY_PATH="$LD_LIBRARY_PATH:$progdir:/usr/trimui/lib"
# Use bundled certs if present, otherwise fall back to system certs
if [ -f "$SIDEB_RESOURCES_DIR/ca-certificates.crt" ]; then
    export SSL_CERT_FILE="$SIDEB_RESOURCES_DIR/ca-certificates.crt"
elif [ -f /etc/ssl/certs/ca-certificates.crt ]; then
    export SSL_CERT_FILE="/etc/ssl/certs/ca-certificates.crt"
fi

echo 1 > /tmp/stay_awake
echo 1 > /tmp/stay_alive

# Kill any existing instances
killall go-librespot 2>/dev/null
killall sideb 2>/dev/null
sleep 1

# Copy binaries to /tmp (SD card is vfat, can't exec directly)
cp "$progdir/go-librespot" /tmp/go-librespot
cp "$progdir/sideb" /tmp/sideb
chmod +x /tmp/go-librespot /tmp/sideb

# Copy yt-dlp and the bundled audio transcoder if present
[ -f "$progdir/yt-dlp" ] && cp "$progdir/yt-dlp" /tmp/yt-dlp && chmod +x /tmp/yt-dlp
[ -f "$progdir/ffmpeg-lite" ] && cp "$progdir/ffmpeg-lite" /tmp/ffmpeg-lite && chmod +x /tmp/ffmpeg-lite

# Start go-librespot backend
mkdir -p "$SIDEB_DATA_DIR"
/tmp/go-librespot --config_dir "$SIDEB_DATA_DIR" > /tmp/go-librespot.log 2>&1 &
BACKEND_PID=$!

# Start UI
/tmp/sideb 2>/tmp/sideb.log

# Cleanup
kill $BACKEND_PID 2>/dev/null
killall go-librespot 2>/dev/null

rm -f /tmp/stay_awake
rm -f /tmp/stay_alive
