#!/bin/sh
progdir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
cd "$progdir" || exit 1

export SIDEB_APP_DIR="$progdir"
export SIDEB_DATA_DIR="${SIDEB_DATA_DIR:-$progdir/data}"
export SIDEB_RESOURCES_DIR="${SIDEB_RESOURCES_DIR:-$progdir/resources}"
export LD_LIBRARY_PATH="$LD_LIBRARY_PATH:$progdir:/usr/trimui/lib"

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

cp "$progdir/go-librespot" /tmp/go-librespot
cp "$progdir/sideb" /tmp/sideb
chmod +x /tmp/go-librespot /tmp/sideb

[ -f "$progdir/yt-dlp" ] && cp "$progdir/yt-dlp" /tmp/yt-dlp && chmod +x /tmp/yt-dlp
[ -f "$progdir/ffmpeg" ] && cp "$progdir/ffmpeg" /tmp/ffmpeg && chmod +x /tmp/ffmpeg

mkdir -p "$SIDEB_DATA_DIR"
/tmp/go-librespot --config_dir "$SIDEB_DATA_DIR" > /tmp/go-librespot.log 2>&1 &
BACKEND_PID=$!

for i in 1 2 3 4 5 6 7 8 9 10; do
    if curl -s http://127.0.0.1:3678/status >/dev/null 2>&1; then
        break
    fi
    sleep 1
done

/tmp/sideb 2>/tmp/sideb.log

kill "$BACKEND_PID" 2>/dev/null
killall go-librespot 2>/dev/null

rm -f /tmp/stay_awake
rm -f /tmp/stay_alive
