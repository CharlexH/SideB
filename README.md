# TrimUI Spotify Connect

A Spotify Connect receiver for the [TrimUI Brick](https://trimui.com) handheld gaming device. Turns your TrimUI Brick into a Spotify Connect speaker with a real-time album art display and hardware button controls.

## Features

- Appears as a Spotify Connect device ("TrimUI Brick") on your local network
- Displays album cover art, track name, and artist name on the 1024x768 screen
- Hardware button controls:
  - **A** — Play / Pause
  - **B** / **MENU** — Exit
  - **Left** / **Right** — Previous / Next track
  - **Up** / **Down** — Volume up / down
- Real-time metadata updates via WebSocket
- Pure Go framebuffer rendering — no SDL2 or X11 required
- Double-buffered display for flicker-free rendering

## Requirements

- TrimUI Brick with CrossMix OS 1.1.1+
- Wi-Fi connection on the same network as your Spotify client
- Spotify Premium account

## Architecture

```
┌─────────────────────┐     ┌──────────────────────┐
│   Spotify App        │     │   TrimUI Brick        │
│   (Phone/Desktop)    │────▶│                       │
│                      │     │  go-librespot (3678)  │
│   Spotify Connect    │     │    ▲           │      │
└─────────────────────┘     │    │ WebSocket  │ ALSA │
                             │    │           ▼      │
                             │  spotify-ui ──▶ Audio │
                             │    │                  │
                             │    ▼                  │
                             │  /dev/fb0 (display)   │
                             └──────────────────────┘
```

- **[go-librespot](https://github.com/devgianlu/go-librespot)** — Open-source Spotify Connect receiver with HTTP API and WebSocket events
- **spotify-ui** — Pure Go framebuffer UI that renders album art and handles input via `/dev/input/event*`

## Installation

### Download Release

Download the latest release archive and extract it to your SD card:

```
/mnt/SDCARD/Apps/SpotifyConnect/
├── config.json          # TrimUI app metadata
├── launch.sh            # Startup script
├── go-librespot         # Spotify Connect backend
├── spotify-ui           # Framebuffer UI
├── data/
│   └── config.yml       # go-librespot config
└── resources/
    ├── ca-certificates.crt  # Mozilla CA bundle
    └── font.ttf             # UI font
```

### Build from Source

**Prerequisites:** Go 1.21+

```bash
# Clone
git clone https://github.com/CharlexH/trimui-spotify.git
cd trimui-spotify

# Build UI binary (cross-compile for ARM64)
cd spotify-ui
GOOS=linux GOARCH=arm64 CGO_ENABLED=0 go build -ldflags="-s -w" -o ../build/spotify-ui .

# Download go-librespot ARM64 binary from:
# https://github.com/devgianlu/go-librespot/releases/latest

# Download CA certificates:
curl -o resources/ca-certificates.crt https://curl.se/ca/cacert.pem

# Copy the full SpotifyConnect folder to your SD card:
# /mnt/SDCARD/Apps/SpotifyConnect/
```

## Usage

1. Launch **Spotify** from the TrimUI Brick app menu
2. The app will start the backend and wait for a Spotify Connect connection
3. Open Spotify on your phone or computer
4. Tap the **Connect** icon and select **"TrimUI Brick"**
5. Music plays through the TrimUI Brick speaker; album art and controls appear on screen

## Configuration

Edit `data/config.yml` to customize go-librespot settings:

```yaml
device_name: "TrimUI Brick"    # Name shown in Spotify Connect
device_type: "speaker"
audio_backend: "alsa"
audio_device: "default"
bitrate: 160                   # 96, 160, or 320 kbps
volume_steps: 100
initial_volume: 80
```

See [go-librespot documentation](https://github.com/devgianlu/go-librespot) for all options.

## Technical Details

- **Display**: Direct framebuffer rendering to `/dev/fb0` (1024x768, BGRA 32bpp)
- **Input**: Linux evdev from `/dev/input/event3` (gamepad) and `/dev/input/event0` (keyboard)
- **Audio**: ALSA via go-librespot
- **Network**: Zeroconf/mDNS for device discovery, HTTP API on port 3678
- **No CGO**: Pure Go cross-compilation — `CGO_ENABLED=0 GOOS=linux GOARCH=arm64`

## Acknowledgments

- [go-librespot](https://github.com/devgianlu/go-librespot) by devgianlu — the Spotify Connect backend
- [TrimUI](https://trimui.com) — the hardware platform
- [CrossMix OS](https://github.com/cizia64/CrossMix-OS) — the custom firmware

## License

This project is licensed under the Apache License 2.0 — see [LICENSE](LICENSE) for details.

go-librespot is licensed separately under its own terms.
