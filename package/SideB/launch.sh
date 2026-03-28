#!/bin/sh
progdir=$(dirname "$0")
cd "$progdir"

export LD_LIBRARY_PATH="$LD_LIBRARY_PATH:$progdir:/usr/trimui/lib"
export SSL_CERT_FILE="$progdir/resources/ca-certificates.crt"

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

# Copy yt-dlp and ffmpeg-full if present
[ -f "$progdir/yt-dlp" ] && cp "$progdir/yt-dlp" /tmp/yt-dlp && chmod +x /tmp/yt-dlp
[ -f "$progdir/ffmpeg-full" ] && cp "$progdir/ffmpeg-full" /tmp/ffmpeg-full && chmod +x /tmp/ffmpeg-full

# Start go-librespot backend
mkdir -p "$progdir/data"
/tmp/go-librespot --config_dir "$progdir/data" > /tmp/go-librespot.log 2>&1 &
BACKEND_PID=$!

# Wait for API to be ready
for i in 1 2 3 4 5 6 7 8 9 10; do
    if curl -s http://127.0.0.1:3678/status > /dev/null 2>&1; then
        break
    fi
    sleep 1
done

# Start UI
/tmp/sideb 2>/tmp/sideb.log

# Cleanup
kill $BACKEND_PID 2>/dev/null
killall go-librespot 2>/dev/null

rm -f /tmp/stay_awake
rm -f /tmp/stay_alive
