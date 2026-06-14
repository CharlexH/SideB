# SideB

SideB is a retro cassette-style music player for [TrimUI Brick](https://trimui.com) with Spotify Connect, offline favorites, and local MP3 playback.

Latest release: `v1.1.0`

- Local playback now uses SideB's SDL audio callback with bundled `ffmpeg-lite` PCM decoding, replacing the old `aplay` subprocess output path.
- Downloads now persist pending work, resume automatically on startup, show clearer progress, and surface short failure notices for cookies, network, storage, YouTube challenge, and missing runtime tools.
- Local MP3 import now ignores macOS metadata files, supports album-folder `cover.*`/`folder.*` art, and backfills missing covers for existing managed tracks.

## Screenshots 📸

<table>
  <tr>
    <td align="center">
      <img src="screenshots/spotify_connecting.png" alt="Spotify connecting screen" width="360"><br>
      <strong>Spotify connecting</strong>
    </td>
    <td align="center">
      <img src="screenshots/favlist.png" alt="Favorites list" width="360"><br>
      <strong>FAV LIST</strong>
    </td>
  </tr>
  <tr>
    <td align="center">
      <img src="screenshots/offline_playing.png" alt="Offline local playback" width="360"><br>
      <strong>Offline playback</strong>
    </td>
    <td align="center">
      <img src="screenshots/waiting.png" alt="Waiting screen" width="360"><br>
      <strong>Waiting screen</strong>
    </td>
  </tr>
</table>

## Features ✨

### Spotify streaming 🎧

- Turns the TrimUI Brick into a Spotify Connect receiver on your local network
- Shows cover art, playback state, and cassette animation directly on `/dev/fb0`
- Supports hardware controls for play, pause, skip, volume, favorites, and list navigation

### Offline FAV list 💾

- Save the current track with `X`
- Browse and manage favorites in the fullscreen `FAV LIST`
- Play downloaded favorites locally with shuffle, previous, and next track controls

### Local uploads 📥

- Drop MP3 files into `data/imports/`
- SideB imports tags and cover art automatically
- Imported tracks are added to `FAV LIST` and behave like local favorites

## How It Works 🧠

The app consists of two components:

- **[go-librespot](https://github.com/devgianlu/go-librespot)**: Spotify Connect backend — handles authentication, audio streaming, and exposes a local HTTP/WebSocket API on port `3678`
- **spotify-ui-rs** (this repo): Rust framebuffer UI — reads input from `/dev/input`, renders the cassette scene to `/dev/fb0`, and communicates with `go-librespot` via its local API

### Offline playback pipeline

When a user marks a track as a favorite, the app searches for multiple matching candidates on YouTube using [yt-dlp](https://github.com/yt-dlp/yt-dlp), scores them by duration match against Spotify metadata, title similarity, and channel quality, then downloads the best match as an MP3 file on the SD card. After download, the actual file duration is validated against the Spotify track length to reject mismatched results. Downloads use a bundled FFmpeg-compatible audio transcoder with MP3 encoder support. Cached audio is decoded by bundled `ffmpeg-lite` into PCM and played through SideB's SDL audio callback. Cover art is fetched from the Spotify CDN or copied from the local cover cache. Incomplete downloads are automatically resumed on the next app launch.

For manual local playback, users can also drop MP3 files into `data/imports/`. SideB scans that folder automatically, reads MP3 metadata, moves the file into `data/music/`, extracts embedded cover art with the device's system ffmpeg when available, and adds the track to `FAV LIST` as a managed local item.

**Important**: The app does **not** intercept, decrypt, or extract audio from Spotify streams. Spotify playback and offline caching use entirely separate audio paths.

## Offline Playback & Legal Notice ⚠️

This project provides an offline caching mechanism strictly for **personal, non-commercial use**. By using this feature, you acknowledge and agree to the following:

- **No content is hosted or distributed by this project.** All audio files are cached locally on the user's own device and are never uploaded, shared, or made available to third parties.
- **The project does not circumvent any DRM or copy protection.** Spotify audio streams are not intercepted or recorded. Offline caching relies on publicly available content retrieved via [yt-dlp](https://github.com/yt-dlp/yt-dlp) from YouTube.
- **Users are solely responsible for ensuring their use complies with applicable copyright laws and the terms of service of any third-party platform** (including YouTube and Spotify). The authors and contributors of this project assume no liability for how the software is used.
- **If you are a rights holder** and believe this project facilitates infringement of your rights, please [open an issue](https://github.com/CharlexH/SideB/issues) and we will address it promptly.

This software is provided as-is for educational and personal use. It is not intended to promote or facilitate unauthorized copying or distribution of copyrighted material.

## Requirements ✅

- TrimUI Brick
- NextUI, Stock OS, or [CrossMix OS](https://github.com/cizia64/CrossMix-OS) `1.1.1+`
- Spotify Premium account
- Wi-Fi on the same network as your Spotify client (for streaming mode)

## Platform Support

- Stable: NextUI, Stock OS, CrossMix OS
- Candidate: KNULLI, muOS

Candidate packages are installable test builds for device validation. They are
not supported releases until real-device proof confirms launch, display, input,
Spotify Connect visibility, local playback, and clean exit back to the OS
frontend.

## Controls 🎮

| Button | Action |
|--------|--------|
| **A** | Play / Pause |
| **← / →** | Previous / Next track |
| **↑ / ↓** | Volume up / down |
| **X** | Favorite, or press twice to remove a favorite |
| **Y** | Open / close playlist |
| **B** / **MENU** | Exit app |

## Usage 🚀

### Spotify streaming

1. Launch **SideB** from the TrimUI app menu.
2. Open Spotify on your phone, desktop, or tablet.
3. Pick **TrimUI Brick** from Spotify Connect.
4. Use the hardware buttons on the device for playback, volume, favorites, and list access.

### Offline FAV list

1. Start playback from Spotify Connect.
2. Press **X** to add the current track to favorites.
3. Wait for the background download to finish.
4. Press **A** on the device to start local playback from the downloaded library, or press **Y** to pick a downloaded track from `FAV LIST`.

### Local uploads

Copy `.mp3` files into the `data/imports/` folder inside the app directory, either directly or inside subfolders:

```text
NextUI   -> /mnt/SDCARD/Tools/tg5040/SideB.pak/data/imports/
Stock    -> /mnt/SDCARD/Apps/SideB/data/imports/
CrossMix -> /mnt/SDCARD/Apps/SideB/data/imports/
```

SideB scans that folder recursively at startup and while running. Imported files are moved into `data/music/`, then added to `FAV LIST` and treated like downloaded local tracks. macOS metadata such as `__MACOSX/`, hidden folders, and `._*.mp3` files is ignored.

#### Cover art for local uploads

For best results, embed cover art in the MP3 before copying it into `data/imports/`. SideB uses embedded cover art first.

If the MP3 has no embedded cover, place a same-name `.jpg`, `.jpeg`, or `.png` image next to the MP3 in `data/imports/`, or in the same import subfolder:

```text
data/imports/My Album/
  Artist - Song.mp3
  Artist - Song.jpg
```

For album subfolders, `cover.jpg`, `cover.jpeg`, `cover.png`, `folder.jpg`, `folder.jpeg`, and `folder.png` are also supported as folder-level covers:

```text
data/imports/My Album/
  01 First Song.mp3
  02 Second Song.mp3
  cover.jpg
```

Folder-level covers only apply inside import subfolders such as `data/imports/My Album/`. `data/imports/cover.jpg` is not treated as a root import default, and `data/music/cover.jpg` is not treated as a global library cover.

If a track was already imported without cover art, place the same-name image next to the managed MP3 in `data/music/`:

```text
data/music/
  Artist - Song.mp3
  Artist - Song.jpg
```

SideB scans for these existing-library covers at startup and while running. In `data/music/`, the image filename still needs to match the MP3 filename stem.

Notes:

- Current manual import support is `MP3` only.
- Embedded cover art is preferred. If no embedded cover exists, SideB tries a same-name sidecar image, then an album-subfolder `cover.*`, then `folder.*`.
- Press **X** once to show the remove confirmation, then press **X** again to actually remove the track from favorites.
- If you remove the track that is currently playing locally, SideB keeps the managed file until playback moves away from that track. If you favorite it again before switching tracks, the cached file is preserved.

## Troubleshooting: Downloads Failing

When a background download finally fails, SideB shows a short notice on the device and writes details to `/tmp/sideb.log`.

| Notice | Common log pattern | Meaning | Action |
|--------|--------------------|---------|--------|
| `Cookie check failed` | `Sign in to confirm you're not a bot`, `Use --cookies` | YouTube wants valid account cookies | Export Firefox cookies to `data/yt-dlp-cookies.txt` |
| `YouTube challenge` | `Signature solving failed`, `n challenge`, `GVS PO Token`, `PO Token` | YouTube changed or gated this format/client path | Retry later, update SideB/yt-dlp in the next package, or try another track |
| `Network error` | DNS, timeout, TLS, SSL, `Network is unreachable` | Device network, Wi-Fi, certificate, proxy, or routing issue | Check device Wi-Fi/router/proxy; the device does not inherit your computer proxy automatically |
| `Storage full` | `No space left on device`, `PyInstaller`, `failed to extract`, `/tmp` | Temporary extraction or SD-card storage failed | Free space, restart SideB to clear `/tmp/sideb-tmp`, then retry |
| `Missing yt-dlp` | `yt-dlp` missing or permission denied | Runtime payload is missing or not executable | Reinstall the full release zip |
| `Audio tool failed` | `ffmpeg`, `ffprobe`, `ffmpeg-location`, `transcoder`, `libmp3lame`, `s16le` | Bundled audio transcoder path or codec support is missing or broken | Reinstall the full release zip and verify `ffmpeg-lite` is present |
| `No audio match` | `Requested format is not available`, duration mismatch | Candidate audio did not match the Spotify track | Try another track or retry later |
| `Download failed` | Other extractor failure | Unknown download failure | Check `/tmp/sideb.log` and open an issue with the log snippet |

For cookie-related failures:

**Use Firefox** (recommended — Chrome rotates cookies on export, making them expire immediately):

1. Install [Firefox](https://www.mozilla.org/firefox/) and log into YouTube
2. Install [yt-dlp](https://github.com/yt-dlp/yt-dlp) (`brew install yt-dlp` / `winget install yt-dlp`)
3. Export cookies and copy to the device:
   ```bash
   yt-dlp --cookies-from-browser firefox --cookies cookies.txt -s "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
   ```
4. Copy `cookies.txt` to the device SD card as `data/yt-dlp-cookies.txt`

Restart SideB and pending downloads will resume automatically.

> **Why not Chrome?** Since late 2024, Chrome automatically invalidates exported cookies as a security measure. Firefox does not have this limitation.

## Build 🔧

### Rust UI (current)

```bash
git clone https://github.com/CharlexH/SideB.git
cd SideB
rustup target add aarch64-unknown-linux-gnu
brew install zig
cargo install cargo-zigbuild --locked
./scripts/package.sh
```

### Required runtime files (not tracked in git)

- `package/SideB.pak/go-librespot` — Spotify Connect backend binary
- `package/SideB.pak/ffmpeg-lite` — bundled FFmpeg-compatible audio transcoder with MP3 encoding and local playback PCM output support; keep it minimal and audio-focused
- `package/SideB.pak/yt-dlp` — YouTube audio downloader (aarch64 binary)
- `package/SideB.pak/resources/ca-certificates.crt` — TLS root certificates

`package/SideB.pak/resources/font_mono.ttf` is tracked and should already be present after checkout.

## Package Releases 📦

Build all release archives:

```bash
./scripts/package.sh
```

This produces:

- `dist/SideB-<version>-nextui.zip`
- `dist/SideB-<version>-stock.zip`
- `dist/SideB-<version>-crossmix.zip`

`scripts/package.sh` also checks release metadata before building: `Cargo.toml`, `pak.json` version, `pak.json` `release_filename`, `pak.json` changelog, and README release labels must all agree. After packaging, it verifies the three platform zips and required runtime payload entries.

Build experimental Candidate packages:

```bash
./scripts/package_candidates.sh
```

This produces:

- `dist/SideB-<version>-knulli-candidate.zip`
- `dist/SideB-<version>-muos-candidate.muxapp`

Candidate artifacts are not part of the Stable release gate. They are intended
for testers who can provide firmware version, package hash, install path,
runtime logs, and on-device proof. Use the checklist in
[`packaging/candidate-test-checklist.md`](packaging/candidate-test-checklist.md)
when reporting Candidate results.

Archive layouts:

- `nextui`: pak contents such as `launch.sh`, `sideb`, `resources/...`, and `data/...`; the NextUI Pak Store extracts this archive into `Tools/tg5040/SideB.pak/`
- `stock`: `Apps/SideB/...`
- `crossmix`: `Apps/SideB/...`
- `knulli-candidate`: PortMaster-style `sideb/...` tree with `SideB.sh`,
  PortMaster metadata, and a `sideb/` payload directory
- `muos-candidate`: `.muxapp` archive with a `SideB/...` application tree,
  `mux_launch.sh`, muOS metadata, and the SideB payload

Supported in this release:

- `NextUI` — validated on device
- `Stock` — package layout and launcher verified
- `CrossMix` — package layout and launcher verified
- `KNULLI` — Candidate package only; not validated on device
- `muOS` — Candidate package only; not validated on device

## Deploy 🚀

Manual installation:

```text
NextUI   -> extract the nextui archive into /mnt/SDCARD/Tools/tg5040/SideB.pak/
Stock    -> extract the stock archive at the SD-card root so it creates /mnt/SDCARD/Apps/SideB/
CrossMix -> extract the crossmix archive at the SD-card root so it creates /mnt/SDCARD/Apps/SideB/
KNULLI   -> Candidate only: extract the knulli archive into the KNULLI roms/ports folder
muOS     -> Candidate only: copy the .muxapp archive to ARCHIVE and install it with Archive Manager
```

On Stable platforms, launch **SideB** from the TrimUI app menu, then select
**TrimUI Brick** from Spotify Connect on another device.

On KNULLI Candidate builds, launch **SideB Candidate** from Ports or
PortMaster after extracting the archive. On muOS Candidate builds, launch
**SideB Candidate** from Applications after installing the `.muxapp` through
Archive Manager.

## GitHub Release 🏷️

Public releases attach three installable archives:

- `SideB-<version>-nextui.zip`
- `SideB-<version>-stock.zip`
- `SideB-<version>-crossmix.zip`

Experimental Candidate releases may also attach:

- `SideB-<version>-knulli-candidate.zip`
- `SideB-<version>-muos-candidate.muxapp`

KNULLI and muOS Candidate builds are unvalidated test packages. They need
tester reports before Beta or Stable support.

The NextUI Pak Store consumes the `nextui` archive via [`pak.json`](pak.json).

Before publishing, verify the GitHub release assets after upload:

```bash
./scripts/check_github_release_assets.sh v<version>
```

Current release tag: `v1.1.0`

## Repo Layout 🗂️

```text
spotify-ui-rs/                  Rust UI source
package/SideB.pak/              Local runtime staging folder and source assets
package/SideB.pak/data/         Runtime config copied into release packages
package/SideB.pak/resources/    UI images, icons, and fonts
packaging/                      Platform wrappers and release metadata
scripts/package.sh              Multi-platform release packager
scripts/check_release_metadata.sh Release metadata consistency gate
scripts/check_github_release_assets.sh GitHub release asset gate
```

## Configuration ⚙️

Main config: [`package/SideB.pak/data/config.yml`](package/SideB.pak/data/config.yml)

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

## Credits & Third-Party Software 🙏

This project builds upon the following open-source projects:

| Project | License | Usage |
|---------|---------|-------|
| [go-librespot](https://github.com/devgianlu/go-librespot) | GPL-3.0 | Spotify Connect backend |
| [yt-dlp](https://github.com/yt-dlp/yt-dlp) | Unlicense | YouTube audio search and download for offline caching |
| [FFmpeg](https://ffmpeg.org/) | LGPL-2.1+ / GPL-2.0+ | Audio decoding and playback pipeline |
| [CrossMix OS](https://github.com/cizia64/CrossMix-OS) | — | Firmware base for TrimUI devices |

Hardware: [TrimUI Brick](https://trimui.com)

Release archives also include:

- `LICENSES/NOTICE.md`
- `LICENSES/THIRD_PARTY_SOURCES.md`
- Full upstream license texts for bundled binaries

Before publishing a GitHub release, record the exact upstream versions and checksums of the bundled third-party binaries in `LICENSES/THIRD_PARTY_SOURCES.md`.

## License 📄

Apache-2.0. See [LICENSE](LICENSE).

## Disclaimer ⚖️

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED. THE AUTHORS ARE NOT RESPONSIBLE FOR ANY MISUSE OF THIS SOFTWARE OR FOR ANY VIOLATION OF THIRD-PARTY TERMS OF SERVICE OR APPLICABLE LAWS. USE AT YOUR OWN RISK.
