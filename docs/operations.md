# Operations And Troubleshooting

This document is for day-to-day work on a real device: build, deploy, swap resources, inspect logs, and remember the current known issues.

## Local verification

Run both test suites before deploying:

```bash
cd spotify-ui-rs
cargo test

cd ../spotify-ui
go test ./...
```

Typical Rust release build:

```bash
cd spotify-ui-rs
cargo build --release --target aarch64-unknown-linux-musl
cp target/aarch64-unknown-linux-musl/release/sideb ../package/SideB/sideb
```

## Device paths

Important runtime paths on the TrimUI device:

- app folder: `/mnt/SDCARD/Apps/SideB`
- runtime UI binary: `/tmp/sideb`
- runtime backend binary: `/tmp/go-librespot`
- runtime yt-dlp binary: `/tmp/yt-dlp`
- runtime static ffmpeg: `/tmp/ffmpeg-full`
- UI log: `/tmp/sideb.log`
- backend log: `/tmp/go-librespot.log`
- cover cache: `/tmp/sideb-cover-cache`
- favorites database: `/mnt/SDCARD/Apps/SideB/data/favorites.json`
- downloaded music: `/mnt/SDCARD/Apps/SideB/data/music/`

## Launch and restart behavior

`launch.sh` does all process startup. Re-running it is the easiest way to restart the app after replacing binaries or resources.

Startup sequence:

1. stale processes are killed
2. binaries are copied to `/tmp` (`go-librespot`, `sideb`, `yt-dlp`, `ffmpeg-full`)
3. backend starts
4. local API is probed
5. UI starts

If resources change but the binary does not, restart the app anyway so cached precomputed render assets are rebuilt.

## Resource update workflow

Common files replaced during iteration:

- `tapeBase.png`
- `tapeA.png`
- `cover_mask.png`
- `taperoll.png`
- `wheel.png`
- `play.png`
- `pause.png`
- `font_mono.ttf`
- `ic-sideb.png`

Notes:

- launcher icon format on the device is `300x300` `PNG RGBA`
- replacing image or font assets usually requires restarting the app
- resources are read from the packaged app folder, not from `/tmp`

## Logs to inspect first

UI and playback issues should start with:

```bash
tail -n 200 /tmp/sideb.log
tail -n 200 /tmp/go-librespot.log
curl -s http://127.0.0.1:3678/status
```

If the app is running, also check:

```bash
ps | grep -E 'sideb|go-librespot'
ls -l /tmp/sideb-cover-cache
```

## Cover switching notes

This area regressed during the Rust rewrite and already consumed a lot of debugging time.

Current expected behavior:

- uncached next cover: blank state while fetch is pending
- cached next cover: direct swap without a visible blank frame
- previous cover should never stay visible while the next uncached track is loading

Relevant current implementation details:

- cover URLs are upgraded to `640x640` Spotify CDN variants
- covers are cached under `/tmp/sideb-cover-cache`
- logs include cover keys and timing markers such as `cache hit`, `fetch done`, `decoded`, `applied from cache`, and `render-state lock waited`

If cover switching feels slow:

- `cache hit` followed much later by `applied from cache` points to `RenderState` lock contention
- long delays before `fetch done` point to network or `curl` behavior
- stale cover reappearing usually means cover state management regressed

## Time synchronization notes

The UI no longer trusts only local elapsed time.

Current correction strategy:

- every `5s` while actively playing
- `750ms` polling for `3s` after `active`, `metadata`, `seek`, or `playing`
- every `1s` during the last `10s` of a track

This was added because:

- remote seeks from a phone or desktop did not reliably update the device UI
- local time could reach `0:00` while playback continued

If time drift regresses, search the UI log for:

- `status sync corrected position`

## Crash history worth remembering

One real crash came from grayscale JPEG cover art.

Symptom:

- panic in `types.rs::pixel_at()`
- root cause was not a corrupt file but a `640x640` grayscale Spotify cover

Fix:

- [`resources.rs`](../spotify-ui-rs/src/resources.rs) now expands grayscale JPEG pixels into RGBA before they reach render code

If cover-related crashes return, inspect the decoded cover format before assuming the image file is bad.

## Local playback and favorites pitfalls

These bugs were encountered during the local playback feature implementation and are documented here so they don't regress.

### Audio overlap between Spotify and local playback

Any code path that starts local audio **must** call `network::api_post("/player/pause")` first. This applies to:

- `StartLocalPlayback` (pressing A on waiting screen)
- `PlaylistSelect` (pressing A on a playlist item)
- App exit (to prevent Spotify continuing after the UI closes)

The go-librespot backend does not know about local playback. It will happily keep streaming Spotify audio unless explicitly paused via its HTTP API.

### Heart icon not updating on track change

The favorites state (`is_favorited`) is stored in `AppState` but the network module (`network.rs`) does not have access to `FavoritesManager`. When Spotify changes tracks via the `metadata` WebSocket event, the network module cannot directly check if the new track is favorited.

Solution: the metadata handler sends `SpotifyTrackChanged` through the command channel, and the command processor queries `FavoritesManager` and updates the heart state. Any new track-change path must also update the favorited state.

### Cover art not saved when favoriting from Spotify

`AppState` does not store the current cover URL — that lives in `RenderState.requested_cover_url`. When building a `FavoriteEntry` during `ToggleFavorite`, the cover URL must be read from `RenderState`, not from `AppState`.

If `cover_url` is empty in a favorite entry, the download thread has no URL to download cover art, and local playback will show no cover. The fallback chain is:

1. Local `.jpg` file alongside the MP3 (written during download)
2. Spotify cover cache at `/tmp/sideb-cover-cache/` (matched by FNV hash of URL)
3. Fetch from Spotify CDN via `curl`

All three require a non-empty `cover_url` to function.

### Playlist overlay requires full redraw on dismiss

The playlist overlay paints over the entire screen. When dismissed, if only dirty rects are updated, the cassette scene appears incomplete. The render loop tracks `last_playlist_visible` and forces `full_redraw = true` when the overlay transitions from visible to hidden.

### Device ffmpeg has no MP3 encoder

The device's built-in ffmpeg (6.1) was compiled with decoders only — no `libmp3lame`. yt-dlp post-processing fails with `"audio conversion failed: Encoder not found"`. A separate static ffmpeg binary with full codec support is shipped at `/mnt/SDCARD/Apps/SideB/ffmpeg-full` and passed to yt-dlp via `--ffmpeg-location`.

### SD card vfat execution restriction

The SD card is mounted as vfat which cannot execute binaries. All binaries (`sideb`, `go-librespot`, `yt-dlp`, `ffmpeg-full`) must be copied to `/tmp` before execution. The `launch.sh` script handles this.

### Spotify events must not mutate UI state during local playback

The go-librespot backend keeps sending WebSocket events and `/status` responses even when the UI has switched to Local mode. Without guards, these events overwrite the local playback state:

- `paused` / `not_playing` / `will_pause` events set `paused = true`, killing wheel/taperoll animation
- `playing` / `will_play` events set `paused = false`, conflicting with local pause state
- `stopped` / `inactive` events clear track name, cover, and `connected`, causing a flash to the waiting scene
- `/status` polling (every 5s) calls `apply_track_snapshot()` which sets `connected = true` and overwrites `paused`, pulling the UI back to Spotify mode

All of these code paths in `network.rs` now check `st.mode != AppMode::Local` before mutating UI state. The `SpotifyDeactivated` command is still sent regardless, so the command processor can handle mode transitions.

If new event types are added to the WebSocket handler, they must include the same Local mode guard.

### Render function must use mode, not connected flag

The `render()` function in `render.rs` originally checked `st.connected` to decide whether to show the cassette animation or the waiting scene. During Local mode `connected` is false (no Spotify session), so animations were skipped.

The fix was to check `st.mode == AppMode::Waiting` instead of `!connected`. Both Spotify and Local modes should render the full cassette scene with wheel/taperoll frames.

### SpotifyDeactivated must restore paused state

When Spotify deactivates and local playback resumes, the `SpotifyDeactivated` handler must explicitly set `paused = false`. The local player is resumed via `SIGCONT`, but the UI `paused` flag was left in whatever state Spotify set it to (usually `true` from a preceding `paused`/`stopped` event).

The handler must also check whether mode is already Local before switching to Waiting — if the user is playing locally and Spotify disconnects independently, the mode should not change.

### Shuffled playback must start with the displayed track

At startup, the app shows `downloaded[0]` in Local paused mode. When the user presses A, `start_shuffled()` shuffles the entire playlist and plays `playlist[0]` — which is now a random track, not the one the user sees.

Fix: `start_shuffled_with_first(entries, first_uri)` shuffles the playlist then swaps the requested URI to index 0. The `TogglePlayPause` handler reads `current_track_uri` from `AppState` (set during startup) and passes it so the displayed track always plays first.

### Initial Local mode must fully initialize track state

When the app starts in Local mode (paused, showing first downloaded track), all track-related state must be set — not just `track_name` and `artist_name`. Missing fields cause:

- Empty `current_track_uri` → heart icon check fails, favorites toggle breaks
- Missing `is_favorited = true` → heart icon shows unfavorited for a track that is in favorites
- Missing cover art → cassette scene shows blank cover area

The startup code must set: `track_name`, `artist_name`, `current_track_uri`, `duration`, `position`, `is_favorited`, and call `load_local_cover()` to decode and apply the cover image to `RenderState`.

### B button must always exit the app

Previously, pressing B in Local mode sent `StopLocalPlayback` which transitioned to Waiting. This trapped the user — they expected B to exit the app (matching MENU behavior). With the mode flow redesign where Waiting is only for fav=0 initialization, B should always send `ExitApp` regardless of mode.

### StopLocalPlayback should stay in Local, not go to Waiting

When local playback ends naturally (track finishes, no more tracks), the mode should remain Local (paused) rather than switching to Waiting. Waiting is reserved for the fav=0 empty state. Going to Waiting mid-session is confusing because the user would need to press A again to re-enter Local, and the cassette scene disappears.

### TogglePlayPause must handle inactive player

When the local player has no active subprocess (initial paused state, or after all tracks finish), `toggle_pause()` does nothing — it only sends signals to running processes. The handler must check `player.is_active()` first. If inactive, start playback instead of toggling.

## Bottom status bar redesign notes

The bottom bar was redesigned from text-based status to icon-based:

Old layout: 60x60 playing/paused lamp + "PLAYING"/"PAUSED" text + procedural heart + time
New layout: spotify_on/off icon + fav_on/off icon + centered track info + play/pause icon + time

New icon assets (all 32x32 PNG RGBA):
- `spotify_on.png` / `spotify_off.png` — Spotify connection state (left side)
- `fav_on.png` / `fav_off.png` — favorite state (left side, only when track loaded)
- `play.png` / `pause.png` — playback state (right side, left of time)

The old `playing.png` and `paused.png` (60x60 lamp icons) are no longer used. The status lamp rendering in `render()` was removed. The asset loader now prefers `play.png`/`pause.png` and falls back to `playing.png`/`paused.png` for backwards compatibility.

The procedural heart drawing (`draw_heart_filled`/`draw_heart_outline`) is no longer called from the bottom bar — replaced by `fav_on`/`fav_off` icons.

## Open improvement ideas

These are the next reasonable performance or stability targets:

- reduce `RenderState` lock contention around cached cover application
- revisit in-process HTTPS cover fetch once the cross-compile toolchain can support it cleanly
- continue debugging the intermittent paused icon draw
- profile render cost if wheel/taperoll smoothness is adjusted again

## Git and branch context

As of the current local state:

- `main` already contains the Rust migration and the first post-migration responsiveness fixes
- the repo is intended to be worked from `main` unless a new focused branch is needed
- if behavior seems surprising, compare against the legacy Go UI before changing backend assumptions
