# TrimUI Spotify Connect

Turn a TrimUI Brick into a Spotify Connect receiver with a native fullscreen UI.  
This project pairs `go-librespot` for playback with a native framebuffer UI for album art, transport state, and hardware button control. The current packaged UI is written in Rust, while the original Go implementation is kept in-tree as a reference. 🎵

## What it does

- 📡 Shows up in Spotify as a Connect device on your local network
- 🖼️ Renders cover art and playback state directly to `/dev/fb0`
- 🎮 Uses TrimUI hardware buttons for play, pause, skip, volume, and exit
- 🔌 Runs without SDL, X11, or a desktop stack
- 🧩 Ships as a simple `Apps/SpotifyConnect` folder for the SD card

## Requirements

- TrimUI Brick
- CrossMix OS `1.1.1+`
- Spotify Premium
- Wi-Fi on the same network as your phone or desktop Spotify client

## Repo layout

```text
spotify-ui/                    Legacy Go UI source
spotify-ui-rs/                 Current Rust UI source
package/SpotifyConnect/        Deployable app folder for the SD card
package/SpotifyConnect/data/   Runtime config and persisted state
package/SpotifyConnect/resources/ UI images and fonts
```

`launch.sh` copies the binaries to `/tmp` before starting them, because the SD card is mounted as `vfat` and can't execute binaries directly.

## Controls

- `A`: Play / pause
- `Left` / `Right`: Previous / next track
- `Up` / `Down`: Volume down / up
- `B` or `MENU`: Exit

## Build

### 1. Build the UI

The deployable `package/SpotifyConnect/spotify-ui` binary should be built from the Rust UI:

```bash
git clone https://github.com/CharlexH/trimui-spotify.git
cd trimui-spotify/spotify-ui-rs
cargo build --release --target aarch64-unknown-linux-musl
cp target/aarch64-unknown-linux-musl/release/spotify-ui ../package/SpotifyConnect/spotify-ui
```

The legacy Go UI is still available for comparison and fallback:

```bash
git clone https://github.com/CharlexH/trimui-spotify.git
cd trimui-spotify/spotify-ui
GOOS=linux GOARCH=arm64 CGO_ENABLED=0 go build -ldflags="-s -w" -o ../package/SpotifyConnect/spotify-ui .
```

### 2. Add `go-librespot`

Download an ARM64 `go-librespot` binary from:

`https://github.com/devgianlu/go-librespot/releases/latest`

Then place it at:

```text
package/SpotifyConnect/go-librespot
```

### 3. Add missing runtime files

The packaged app expects:

- `package/SpotifyConnect/resources/ca-certificates.crt`
- `package/SpotifyConnect/resources/font.ttf`

These are intentionally not tracked in git because they are large third-party assets.

## Deploy

Copy `package/SpotifyConnect` to your SD card:

```text
/mnt/SDCARD/Apps/SpotifyConnect/
```

The final folder should contain:

```text
config.json
launch.sh
go-librespot
spotify-ui
data/config.yml
resources/
```

Launch `Spotify` from the TrimUI app menu, then pick `TrimUI Brick` from Spotify Connect on another device. ✅

## Configuration

Main config lives in [`package/SpotifyConnect/data/config.yml`](package/SpotifyConnect/data/config.yml).

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

The UI talks to the local `go-librespot` API at `http://127.0.0.1:3678`.

## Runtime flow

1. `launch.sh` kills any stale process.
2. It copies `go-librespot` and `spotify-ui` to `/tmp`.
3. `go-librespot` starts with `data/config.yml`.
4. The script waits for the local API to come up.
5. `spotify-ui` connects over HTTP/WebSocket and renders the playback UI.

## Debug notes

### Cover switching regression notes

This project hit a noticeable cover-switching regression during the Rust rewrite. The symptom was not just "slow downloads" — already cached covers also failed to appear immediately.

The main causes were:

- The Rust UI originally kept the previous `scene_cover` visible while the next track's cover was still pending. That meant the old album art could stay on screen for seconds.
- Cached covers were being read from disk correctly, but applying them could still block on the shared `render_state` lock. On-device logs showed `cache hit` followed by `applied from cache` several seconds later, which confirmed lock contention rather than cache failure.
- HTTPS cover downloads still use external `curl` on-device. This is slower and less predictable than the old Go UI's in-process HTTP client, but it remains the current implementation because the cross-compile toolchain for `aarch64-unknown-linux-musl` cannot currently absorb a `rustls/ring` dependency without extra linker tooling.

The current Rust behavior should be:

- When track metadata changes, the old cover is cleared immediately instead of lingering on screen.
- If the next cover is already cached in `/tmp/spotify-ui-cover-cache`, it is decoded and applied synchronously.
- Cover logs are written to `/tmp/spotify-ui.log` with `cache hit`, `fetch done`, `decoded`, `applied`, and `render-state lock waited` markers so device-side timing can be diagnosed without rebuilding.

If cover switching regresses again, inspect:

```bash
tail -n 200 /tmp/spotify-ui.log
ls -l /tmp/spotify-ui-cover-cache
curl -s http://127.0.0.1:3678/status
```

If the log shows `cache hit` followed much later by `applied from cache`, the problem is render-state contention, not network fetch latency.

## Credits

- [`go-librespot`](https://github.com/devgianlu/go-librespot) for the Spotify Connect backend
- [TrimUI](https://trimui.com) for the hardware
- [CrossMix OS](https://github.com/cizia64/CrossMix-OS) for the firmware base

## License

Apache-2.0. See [LICENSE](LICENSE).
