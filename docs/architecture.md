# Runtime Architecture

This document describes the current shipped architecture, not the original Go-only implementation.

## High-level layout

The app is split into two pieces:

- `go-librespot`: Spotify Connect backend, local playback control, and local HTTP/WebSocket APIs
- `spotify-ui-rs`: native Rust framebuffer UI that renders the cassette scene, cover art, status line, and input-driven controls

The packaged app in [`package/SpotifyConnect`](../package/SpotifyConnect) launches both pieces. The Go UI in [`spotify-ui`](../spotify-ui) is now a reference implementation and fallback, not the default runtime.

## Startup flow

Runtime startup is controlled by [`package/SpotifyConnect/launch.sh`](../package/SpotifyConnect/launch.sh).

1. Kill stale `go-librespot` and `spotify-ui` processes.
2. Copy `go-librespot` and `spotify-ui` from the SD card app folder to `/tmp`.
3. Start `go-librespot` with `package/SpotifyConnect/data/config.yml`.
4. Poll `http://127.0.0.1:3678/status` until the local API is up.
5. Start `/tmp/spotify-ui`.

The copy-to-`/tmp` step is required because the SD card is mounted as `vfat` and cannot execute binaries in place.

## Rust UI thread model

The Rust UI entry point is [`spotify-ui-rs/src/main.rs`](../spotify-ui-rs/src/main.rs).

It starts:

- the main render loop on the main thread
- an input thread for `/dev/input` button events
- a WebSocket thread for `ws://127.0.0.1:3678/events`
- a lightweight status polling thread for periodic position correction
- a command processor thread for dispatching `InputAction` messages
- a download thread for sequential yt-dlp background downloads
- a local playback monitor thread for track-end detection and position updates

Shared state is split into:

- [`AppState`](../spotify-ui-rs/src/app.rs): playback state, track metadata, position, volume, connection state, mode, favorites state, playlist view state
- [`RenderState`](../spotify-ui-rs/src/render.rs): precomputed scene buffers, cover state, animation caches, redraw flags

Both are currently shared through `Arc<Mutex<...>>`. This is good enough on-device, but lock contention in `RenderState` is still one of the first places to inspect when the UI feels sluggish.

### Command processor pattern

All state mutation from input and network threads goes through a single `mpsc::channel<InputAction>` consumed by the command processor thread. This prevents deadlocks from multiple threads locking multiple mutexes simultaneously. The input thread and WebSocket thread are producers; the command processor is the sole consumer that touches `FavoritesManager`, `LocalPlayer`, and `DownloadManager`.

## Application modes

The app operates in three modes defined in [`mode.rs`](../spotify-ui-rs/src/mode.rs):

- **Waiting**: initialization state when no downloaded favorites exist (fav count = 0). Shows waiting screen. This is NOT an intermediate state — once the user has favorites, they never return to Waiting during normal use.
- **Spotify**: Spotify Connect active. Cassette scene with icon-based status bar.
- **Local**: playing or paused on downloaded favorites via `ffmpeg | aplay`. Same cassette scene. This is the default startup mode when downloaded favorites exist — the app starts paused on the first track.

Startup flow: if `downloaded_entries().len() > 0`, the app starts in Local mode (paused) with the first track's metadata, cover, and favorited state fully loaded. Otherwise it starts in Waiting.

Spotify always takes priority: when Spotify becomes active during local playback, local is paused and `local_was_playing` is set. When Spotify deactivates, local resumes if it was playing before.

Important design rule: the go-librespot backend does not know about Local mode. It continues sending WebSocket events (`paused`, `playing`, `stopped`, `inactive`) and responding to `/status` polls even when the UI is in Local mode. All event handlers and status polling in `network.rs` must guard against mutating UI state when `mode == Local`. Without these guards, Spotify events overwrite local playback state (paused flag, connected flag, track info, cover), causing animation freezes, screen flashes, or unintended mode switches.

## Source map

Current Rust modules:

- [`main.rs`](../spotify-ui-rs/src/main.rs): process startup, thread launch, signal setup, command processor
- [`app.rs`](../spotify-ui-rs/src/app.rs): shared playback state, mode, favorites state, and dirty flags
- [`mode.rs`](../spotify-ui-rs/src/mode.rs): `AppMode` enum and `InputAction` command enum
- [`network.rs`](../spotify-ui-rs/src/network.rs): HTTP control, status fetch, event handling, cover fetch/cache logic
- [`render.rs`](../spotify-ui-rs/src/render.rs): scene building, bottom status line, cover swaps, render loop, playlist overlay dispatch
- [`image_ops.rs`](../spotify-ui-rs/src/image_ops.rs): image transforms, cover masking, wheel/taperoll precomputation
- [`resources.rs`](../spotify-ui-rs/src/resources.rs): font/image loading and JPEG decode helpers
- [`input.rs`](../spotify-ui-rs/src/input.rs): mode-aware hardware button routing via `InputAction` channel
- [`framebuffer.rs`](../spotify-ui-rs/src/framebuffer.rs): `/dev/fb0` access
- [`font.rs`](../spotify-ui-rs/src/font.rs): text measurement and drawing
- [`drawing.rs`](../spotify-ui-rs/src/drawing.rs): pixel-level drawing primitives, procedural heart icon
- [`constants.rs`](../spotify-ui-rs/src/constants.rs): screen layout, timing, input constants, data paths
- [`favorites.rs`](../spotify-ui-rs/src/favorites.rs): JSON-backed favorites persistence with atomic save
- [`download.rs`](../spotify-ui-rs/src/download.rs): background yt-dlp download manager with cover art handling
- [`local_player.rs`](../spotify-ui-rs/src/local_player.rs): ffmpeg|aplay subprocess management, pause/resume via signals
- [`playlist_view.rs`](../spotify-ui-rs/src/playlist_view.rs): full-screen playlist overlay rendering

## Local playback pipeline

Local playback uses an `ffmpeg → aplay` subprocess pipeline managed in [`local_player.rs`](../spotify-ui-rs/src/local_player.rs):

```
ffmpeg -i file.mp3 -f s16le -ar 44100 -ac 2 pipe:1 | aplay -f S16_LE -r 44100 -c 2
```

- Pause/resume uses `SIGSTOP`/`SIGCONT` via `libc::kill` on both PIDs
- Track end detection uses `try_wait()` polling every 500ms in the monitor thread
- Position is estimated from wall clock elapsed time with pause accounting
- Shuffle uses Fisher-Yates with xorshift PRNG (no `rand` crate dependency)

## Favorites and download system

Favorites are persisted in `/mnt/SDCARD/Apps/SpotifyConnect/data/favorites.json` with atomic writes (`.tmp` + rename).

Downloads use a standalone `yt-dlp` aarch64 binary with a separate static `ffmpeg` that includes `libmp3lame` (the device's built-in ffmpeg only has decoders, no MP3 encoder):

```
yt-dlp -x --audio-format mp3 --ffmpeg-location /tmp/ffmpeg-full -o "{output}" "ytsearch1:{query}"
```

Cover art for downloads has three sources (checked in order):
1. `curl` download from Spotify CDN URL
2. Copy from Spotify cover cache at `/tmp/spotify-ui-cover-cache/` (original JPEG bytes)
3. Fallback to no cover

## Render pipeline

The render path is optimized around a mostly static scene plus a few dynamic overlays.

Important current behavior:

- `scene_base` contains the shared bottom chrome and static background
- `scene_playing` is rebuilt to include the static cassette layer, masked cover, and foreground
- wheel and taperoll frames are precomputed at startup
- the render loop redraws only the dynamic regions needed for playback state and animation

Current animation configuration in [`constants.rs`](../spotify-ui-rs/src/constants.rs):

- base animation rate: `30 FPS`
- wheel rotation frames: `30`
- taperoll frames per size bucket: `30`
- taperoll size step: `12`

This is intentionally fixed at `30 FPS` right now. Earlier experiments with adaptive `60 FPS` were removed in favor of stability.

## Cover pipeline

Cover handling is centered in [`network.rs`](../spotify-ui-rs/src/network.rs).

Current behavior:

- Spotify CDN cover URLs are upgraded to the `640x640` variant when possible
- cached covers live in `/tmp/spotify-ui-cover-cache`
- cache hits are decoded and applied synchronously
- cache misses clear the current cover and fetch in the background
- stale fetch results are dropped if the requested cover URL changed while the download was in flight

Important design rule:

- `cache hit`: switch directly to the cached cover without showing a blank state
- `cache miss`: show the blank/placeholder state rather than briefly showing the previous track's cover

HTTPS cover downloads still use external `curl` instead of an in-process Rust TLS client. This is a deliberate toolchain tradeoff:

- the Go UI used an in-process HTTP client
- the Rust rewrite initially hit cross-compile friction around adding a Rust TLS stack to `aarch64-unknown-linux-musl`
- for now the project keeps `curl` with strict timeouts and logs instead of adding a more complex linker/toolchain dependency

## Time and playback state synchronization

Playback state is driven by two sources:

- WebSocket events from `/events`
- periodic `/status` polling for drift correction

Current timing rules in [`network.rs`](../spotify-ui-rs/src/network.rs):

- normal playback polling: every `5s`
- boost window after `active`, `metadata`, `seek`, `playing`: every `750ms` for `3s`
- near track end: every `1s` during the last `10s`
- position correction threshold: `800ms` normally, `300ms` during the boost window

This exists because event-only updates were not enough:

- remote seeks from another device did not always update the TrimUI immediately
- local position could hit `0:00` while playback was still active

## Bottom status line

The bottom line is rendered in [`render.rs`](../spotify-ui-rs/src/render.rs).

Current layout (icon-based):

- left side: Spotify connection icon (`spotify_on`/`spotify_off`) + favorite icon (`fav_on`/`fav_off`)
- center: `歌曲名 - 歌手`
- right side: play/pause icon (`play`/`pause`) + time remaining

All status bar icons are 28x28 PNG RGBA, drawn at `BAR_ICON_Y` (653px).

The middle text:

- uses the mono font
- is centered across the full screen width
- is truncated by character count, not pixel width
- currently caps at `30` characters with `…`

## Event handling notes

Current playback event handling includes:

- `playing`
- `will_play`
- `paused`
- `will_pause`
- `not_playing`
- `metadata`
- `seek`
- `volume`

`will_play` and `will_pause` were added because waiting only for `playing` and `paused` made the transport indicator feel delayed.

## Resource files

The packaged app expects these runtime resources in [`package/SpotifyConnect/resources`](../package/SpotifyConnect/resources):

- `tapeBase.png`
- `tapeA.png`
- `cover_mask.png`
- `taperoll.png`
- `wheel.png`
- `play.png` (28x28, bottom bar play indicator)
- `pause.png` (28x28, bottom bar pause indicator)
- `spotify_on.png` (28x28, Spotify connected indicator)
- `spotify_off.png` (28x28, Spotify disconnected indicator)
- `fav_on.png` (28x28, favorited indicator)
- `fav_off.png` (28x28, not favorited indicator)
- `font.ttf`
- `font_mono.ttf`
- `ca-certificates.crt`

Not all of these are tracked in git. The project assumes manual replacement of art assets during iteration.

## Legacy Go UI

The Go UI in [`spotify-ui`](../spotify-ui) is still useful for:

- comparing behavior after Rust regressions
- checking how an older code path handled covers or transport updates
- confirming whether a bug is in `go-librespot` or in the Rust frontend

It should be treated as a reference, not the current shipping implementation.
