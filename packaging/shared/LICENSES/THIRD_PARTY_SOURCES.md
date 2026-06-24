# Third-Party Source Manifest

This manifest records the bundled third-party binaries that ship with SideB release archives.

## Bundled Binaries

### go-librespot

- Upstream: https://github.com/devgianlu/go-librespot
- Bundled version: v0.7.4 (`go-librespot_linux_arm64.tar.gz`)
- Source: https://github.com/devgianlu/go-librespot/tree/v0.7.4
- License: GPL-3.0-only
- Local package path: `package/SideB.pak/go-librespot`
- Bundled binary SHA256: `116743e7617dd60298aded446c6b61f9b8bc2981090a57ca580d77738512c134`
- Upstream archive SHA256: `62b6c7ebee6abb1f59fa3cf20a9374dd5e8c5c1ca5dab329dd312f66b59faa8d`
- Release requirement: keep the source link and bundled checksum in sync with the binary attached to each SideB release.

### yt-dlp

- Upstream: https://github.com/yt-dlp/yt-dlp
- Bundled version: 2026.06.09 (`yt-dlp_linux_aarch64`)
- Source: https://github.com/yt-dlp/yt-dlp/tree/2026.06.09
- License: Unlicense
- Local package path: `package/SideB.pak/yt-dlp`
- Bundled binary SHA256: `cabd246445bdfde0eda0dfe68bbe90354be83f3fdbbf077df11a2ea55f41cdbd`

### FFmpeg

- Upstream: https://ffmpeg.org
- Bundled version: FFmpeg 7.1.1 with LAME 3.100
- Source: https://ffmpeg.org/releases/ffmpeg-7.1.1.tar.xz and https://downloads.sourceforge.net/project/lame/lame/3.100/lame-3.100.tar.gz
- License: GPL-2.0-or-later when built with GPL-only components
- Local package path: `package/SideB.pak/ffmpeg-lite`
- SideB usage: bundled audio transcoder for yt-dlp downloads and local playback PCM output; local embedded-cover extraction uses the device's system ffmpeg
- Bundled binary SHA256: `f140eb53185a8d8cef91b303bfbcd24dbe90a541a22cd5e985355f9768650bc4`
- Build configuration: `--prefix=/opt/sideb-ffmpeg --arch=aarch64 --target-os=linux --pkg-config=pkg-config --pkg-config-flags=--static --extra-cflags=-I/opt/sideb-ffmpeg/include --extra-ldflags=-L/opt/sideb-ffmpeg/lib --extra-libs='-lm -ldl -lpthread' --disable-autodetect --disable-debug --disable-doc --disable-ffplay --disable-ffprobe --disable-network --disable-everything --enable-ffmpeg --enable-static --disable-shared --enable-protocol='file,pipe' --enable-libmp3lame --enable-avcodec --enable-avfilter --enable-avformat --enable-swresample --enable-filter='aresample,aformat' --enable-parser='aac,flac,mpegaudio,opus,vorbis' --enable-demuxer='aac,flac,matroska,mov,mp3,ogg,wav' --enable-decoder='aac,alac,flac,mp3,opus,pcm_s16le,pcm_s24le,vorbis' --enable-muxer='mp3,mov,mp4,ipod,pcm_s16le' --enable-encoder='libmp3lame,pcm_s16le'`
- Release requirement: keep the source links, build configuration, and bundled checksum in sync with the binary attached to each SideB release.

## Release Checklist

For each public GitHub release that bundles these binaries:

1. Record the exact upstream version or commit for every bundled binary.
2. Record the SHA256 checksum for every bundled binary.
3. Attach the corresponding source archive, or provide an equivalent written offer where the license allows it.
4. Keep this manifest in sync with the packaged binaries.
