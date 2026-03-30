# Third-Party Source Manifest

This manifest records the bundled third-party binaries that ship with SideB release archives.

## Bundled Binaries

### go-librespot

- Upstream: https://github.com/devgianlu/go-librespot
- License: GPL-3.0-only
- Local package path: `package/SideB.pak/go-librespot`
- Release requirement: publish the exact corresponding source used for the bundled binary, or provide a valid written offer alongside the release.

### yt-dlp

- Upstream: https://github.com/yt-dlp/yt-dlp
- License: Unlicense
- Local package path: `package/SideB.pak/yt-dlp`
- Recommended binary: `yt-dlp_min_linux_aarch64` (minimal build, ~8MB vs ~25MB full)
- Recommended release metadata: record the upstream version and SHA256 of the binary attached to the release.

### FFmpeg

- Upstream: https://ffmpeg.org
- License: GPL-2.0-or-later when built with GPL-only components
- Local package path: `package/SideB.pak/ffmpeg`
- SideB usage: bundled audio transcoder for yt-dlp downloads; local embedded-cover extraction uses the device's system ffmpeg
- Build notes: minimal audio-only build with libmp3lame encoder (no video codecs required)
- Release requirement: publish the exact corresponding source and build configuration used for the bundled binary, or provide a valid written offer alongside the release.

## Release Checklist

For each public GitHub release that bundles these binaries:

1. Record the exact upstream version or commit for every bundled binary.
2. Record the SHA256 checksum for every bundled binary.
3. Attach the corresponding source archive, or provide an equivalent written offer where the license allows it.
4. Keep this manifest in sync with the packaged binaries.
