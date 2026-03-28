# SideB

Turn a [TrimUI Brick](https://trimui.com) into a Spotify Connect receiver with a native fullscreen cassette-tape UI.

## Features

### Spotify Connect Streaming

- Appears as a Spotify Connect device on your local network
- Streams audio from any Spotify client (phone, desktop, web)
- Renders cover art, playback state, and cassette animation directly to `/dev/fb0`
- Hardware button controls for play, pause, skip, volume, and favorites
- Runs without SDL, X11, or a desktop environment

### Offline Favorites

- Mark any playing track as a favorite (press X)
- Browse and manage favorites in a full-screen playlist overlay (press Y)
- Cached favorites can be played back locally when Wi-Fi is unavailable
- Shuffle playback with prev/next track navigation

## How It Works

The app consists of two components:

- **[go-librespot](https://github.com/devgianlu/go-librespot)**: Spotify Connect backend — handles authentication, audio streaming, and exposes a local HTTP/WebSocket API on port `3678`
- **spotify-ui-rs** (this repo): Rust framebuffer UI — reads input from `/dev/input`, renders the cassette scene to `/dev/fb0`, and communicates with `go-librespot` via its local API

### Offline playback pipeline

When a user marks a track as a favorite, the app searches for a matching publicly available audio source on YouTube using [yt-dlp](https://github.com/yt-dlp/yt-dlp) and caches it locally as an MP3 file on the SD card. Cached audio is played back through an `ffmpeg → aplay` subprocess pipeline. Cover art is fetched from the Spotify CDN or copied from the local cover cache.

**Important**: The app does **not** intercept, decrypt, or extract audio from Spotify streams. Spotify playback and offline caching use entirely separate audio paths.

## Offline Playback & Legal Notice

This project provides an offline caching mechanism strictly for **personal, non-commercial use**. By using this feature, you acknowledge and agree to the following:

- **No content is hosted or distributed by this project.** All audio files are cached locally on the user's own device and are never uploaded, shared, or made available to third parties.
- **The project does not circumvent any DRM or copy protection.** Spotify audio streams are not intercepted or recorded. Offline caching relies on publicly available content retrieved via [yt-dlp](https://github.com/yt-dlp/yt-dlp) from YouTube.
- **Users are solely responsible for ensuring their use complies with applicable copyright laws and the terms of service of any third-party platform** (including YouTube and Spotify). The authors and contributors of this project assume no liability for how the software is used.
- **If you are a rights holder** and believe this project facilitates infringement of your rights, please [open an issue](https://github.com/CharlexH/SideB/issues) and we will address it promptly.

This software is provided as-is for educational and personal use. It is not intended to promote or facilitate unauthorized copying or distribution of copyrighted material.

## Requirements

- TrimUI Brick
- [CrossMix OS](https://github.com/cizia64/CrossMix-OS) `1.1.1+`
- Spotify Premium account
- Wi-Fi on the same network as your Spotify client (for streaming mode)

## Controls

| Button | Action |
|--------|--------|
| **A** | Play / Pause |
| **← / →** | Previous / Next track |
| **↑ / ↓** | Volume up / down |
| **X** | Toggle favorite |
| **Y** | Open / close playlist |
| **B** / **MENU** | Exit app |

## Build

### Rust UI (current)

```bash
git clone https://github.com/CharlexH/SideB.git
cd SideB/spotify-ui-rs
cargo build --release --target aarch64-unknown-linux-musl
cp target/aarch64-unknown-linux-musl/release/sideb ../package/SideB/sideb
```

### Required runtime files (not tracked in git)

- `package/SideB/go-librespot` — Spotify Connect backend binary
- `package/SideB/ffmpeg-full` — static ffmpeg with MP3 encoder support
- `package/SideB/yt-dlp` — YouTube audio downloader (aarch64 binary)
- `package/SideB/resources/ca-certificates.crt` — TLS root certificates
- `package/SideB/resources/font.ttf` — UI font

## Deploy

Copy `package/SideB` to the SD card:

```text
/mnt/SDCARD/Apps/SideB/
```

Launch **SideB** from the TrimUI app menu, then select **TrimUI Brick** from Spotify Connect on another device.

## Repo Layout

```text
spotify-ui-rs/                  Rust UI source
package/SideB/                  Deployable app folder for the SD card
package/SideB/data/             Runtime config and persisted state (favorites, music)
package/SideB/resources/        UI images, icons, and fonts
```

## Configuration

Main config: [`package/SideB/data/config.yml`](package/SideB/data/config.yml)

```yaml
device_name: "TrimUI Brick"
device_type: "speaker"
audio_backend: "alsa"
audio_device: "default"
bitrate: 160
volume_steps: 100
initial_volume: 80
zeroconf_enabled: true
```

The UI communicates with `go-librespot` at `http://127.0.0.1:3678`.

## Credits & Third-Party Software

This project builds upon the following open-source projects:

| Project | License | Usage |
|---------|---------|-------|
| [go-librespot](https://github.com/devgianlu/go-librespot) | GPL-3.0 | Spotify Connect backend |
| [yt-dlp](https://github.com/yt-dlp/yt-dlp) | Unlicense | YouTube audio search and download for offline caching |
| [FFmpeg](https://ffmpeg.org/) | LGPL-2.1+ / GPL-2.0+ | Audio decoding and playback pipeline |
| [CrossMix OS](https://github.com/cizia64/CrossMix-OS) | — | Firmware base for TrimUI devices |

Hardware: [TrimUI Brick](https://trimui.com)

## License

Apache-2.0. See [LICENSE](LICENSE).

## Disclaimer

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED. THE AUTHORS ARE NOT RESPONSIBLE FOR ANY MISUSE OF THIS SOFTWARE OR FOR ANY VIOLATION OF THIRD-PARTY TERMS OF SERVICE OR APPLICABLE LAWS. USE AT YOUR OWN RISK.
