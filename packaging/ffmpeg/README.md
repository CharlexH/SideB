# FFmpeg Lite Build

This directory contains the reproducible Docker recipe for SideB's bundled
audio transcoder.

Runtime contract:

- output binary name: `ffmpeg-lite`
- packaged path: `package/SideB.pak/ffmpeg-lite`
- runtime path on device: `/tmp/ffmpeg-lite`

The build is intentionally narrow. It keeps the features SideB needs for:

- `yt-dlp` post-processing to MP3 with `libmp3lame`
- local MP3 and common audio-file playback through `s16le` PCM output
- local file and pipe protocols only

It does not cover embedded-cover extraction. That remains a best-effort system
`/usr/bin/ffmpeg` path.

To build the binary locally:

```bash
./scripts/build_ffmpeg_audio_transcoder.sh
```

The binary is written to `package/SideB.pak/ffmpeg-lite`. Build metadata and the
artifact checksum are written to `dist/ffmpeg-audio-transcoder/`.

To inspect the pinned versions without running Docker:

```bash
./scripts/build_ffmpeg_audio_transcoder.sh --print-config
```
