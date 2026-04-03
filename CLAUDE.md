# SideB — Claude Code Guidelines

## Project Overview

SideB is a retro cassette-style music player for **TrimUI Brick** (aarch64 embedded Linux). It combines Spotify Connect streaming with offline favorites and local MP3 playback, rendering a cassette UI directly to the framebuffer (`/dev/fb0`).

## Architecture

Two processes work together:
- **go-librespot** — Spotify Connect backend (port 3678, HTTP + WebSocket)
- **sideb** (this repo, `spotify-ui-rs/`) — Rust framebuffer UI that handles input, rendering, downloads, and local playback

Key design: the app does NOT intercept Spotify audio. Offline caching uses yt-dlp to find matching YouTube audio, scored by duration/title/channel quality.

## Build & Test

```bash
# Run tests (host)
cd spotify-ui-rs && cargo test

# Cross-compile for device
cd spotify-ui-rs && cargo build --release --target aarch64-unknown-linux-musl

# Output binary
spotify-ui-rs/target/aarch64-unknown-linux-musl/release/sideb
```

Target is `aarch64-unknown-linux-musl` with static linking (`link-self-contained=yes`, `rust-lld`). Binary size matters — `opt-level = "z"`, LTO enabled, `panic = "abort"`, stripped.

## Deploy to Device

The device SD card is **vfat** — cannot execute binaries directly. `launch.sh` copies binaries to `/tmp` before running.

```bash
# SSH to device
ssh root@192.168.1.113

# Deploy flow: scp → kill → rm old → cp new
scp target/aarch64-unknown-linux-musl/release/sideb root@192.168.1.113:/tmp/sideb_new
# On device: kill old, cp to SD card path, restart via launch.sh
```

Device logs: `/tmp/sideb.log`, `/tmp/go-librespot.log`

## Source Layout (`spotify-ui-rs/src/`)

| File | Purpose |
|------|---------|
| `main.rs` | Event loop, message dispatch, startup |
| `render.rs` | Framebuffer render loop, scene composition |
| `download.rs` | Download manager, multi-candidate search, scoring, progress tracking |
| `playlist_view.rs` | FAV LIST overlay rendering, progress indicators |
| `network.rs` | HTTP/WebSocket communication with go-librespot |
| `local_player.rs` | Offline playback via ffmpeg → aplay pipeline |
| `local_import.rs` | MP3 import from `data/imports/` |
| `favorites.rs` | FavoritesManager, JSON persistence |
| `input.rs` | `/dev/input` event handling |
| `app.rs` | AppState, shared state types |
| `drawing.rs` | Pixel-level drawing primitives |
| `image_ops.rs` | PNG/JPEG decoding, scaling |
| `framebuffer.rs` | `/dev/fb0` mmap interface |
| `constants.rs` | Shared constants |
| `paths.rs` | AppPaths, directory resolution |
| `resources.rs` | Resource loading |
| `animation.rs` | Cassette tape animation |
| `font.rs` | Font rendering via ab_glyph |
| `types.rs` | Shared type definitions |
| `mode.rs` | App mode enum |
| `log_utils.rs` | Logging helpers |

## Key Patterns

### Shared State
`Arc<Mutex<AppState>>` is the central state, shared between main thread, render thread, and download thread. Set `render_dirty = true` to trigger UI redraw.

### Download Pipeline
`DownloadPhase` enum: `Queued → Searching → Downloading(f32) → Transcoding → done`
Progress tracked via `Arc<Mutex<HashMap<String, DownloadPhase>>>`, rendered as pie indicators in playlist view.

### Rendering
Direct framebuffer writes — no GPU, no window system. 1024x600 RGB565/BGRA depending on device. Render loop polls `render_dirty` flag.

## Constraints

- **No new crate dependencies without good reason** — binary size is critical for embedded deployment
- **No std networking beyond what ureq/tungstenite provide** — musl static linking
- **All file I/O must handle vfat limitations** — no symlinks, no execute permission, no case sensitivity
- **yt-dlp writes progress to stdout, not stderr** — important for any pipe-based interaction
- **ffmpeg subprocess inherits parent stdout fd** — avoid pipe-based progress reading; use file-size polling instead

## Release Process

1. Bump version in `Cargo.toml`, `pak.json`, `README.md`
2. Update `pak.json` changelog and `release_filename`
3. Commit, push, create GitHub release via `gh release create`
4. Run `./scripts/package.sh` to build platform archives
5. Attach archives to the release

## Packaging

Three platform variants: `nextui`, `stock`, `crossmix`. Each has its own `launch.sh` and install path layout under `packaging/`. The `pak.json` is consumed by the NextUI Pak Store.
