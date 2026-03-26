#!/bin/sh
progdir=$(dirname "$0")
cd "$progdir"

export LD_LIBRARY_PATH="$LD_LIBRARY_PATH:$progdir:/usr/trimui/lib"
export SSL_CERT_FILE="$progdir/resources/ca-certificates.crt"
export GOTRACEBACK=crash

echo 1 > /tmp/stay_awake
echo 1 > /tmp/stay_alive

# Kill any existing instances
killall go-librespot 2>/dev/null
killall spotify-ui 2>/dev/null
sleep 1

# Copy binaries to /tmp (SD card is vfat, can't exec directly)
cp "$progdir/go-librespot" /tmp/go-librespot
cp "$progdir/spotify-ui" /tmp/spotify-ui
chmod +x /tmp/go-librespot /tmp/spotify-ui

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
/tmp/spotify-ui 2>/tmp/spotify-ui.log

# Cleanup
kill $BACKEND_PID 2>/dev/null
killall go-librespot 2>/dev/null

rm -f /tmp/stay_awake
rm -f /tmp/stay_alive
