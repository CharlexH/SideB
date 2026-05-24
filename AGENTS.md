# Agent Notes

## Bundled Audio Runtime

SideB should use the bundled `ffmpeg-lite` as the controlled audio decode/transcode path. Do not make local playback depend on finding a system `ffmpeg` in `PATH`; behavior needs to stay reproducible across NextUI, Stock, CrossMix, and future TrimUI-style systems.

Runtime contract:

- Packaged binary: `package/SideB.pak/ffmpeg-lite`
- Device runtime path: `/tmp/ffmpeg-lite`
- Required capabilities: `libmp3lame` encoder for downloaded MP3 transcoding and `s16le` muxer for local playback PCM output into `aplay`
- Verification gate: `./scripts/check_ffmpeg_audio_transcoder.sh package/SideB.pak/ffmpeg-lite`

The system `/usr/bin/ffmpeg` path is still a best-effort helper for embedded-cover extraction during import, not the playback path. `aplay` remains the system ALSA output command unless device evidence shows it is missing.

When changing the bundled FFmpeg build, update `packaging/ffmpeg/Dockerfile`, rebuild via `./scripts/build_ffmpeg_audio_transcoder.sh`, refresh `packaging/shared/LICENSES/THIRD_PARTY_SOURCES.md`, and measure package-size impact before release.
