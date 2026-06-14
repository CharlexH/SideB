# Agent Notes

## Platform Support Tiers

SideB has three support tiers. Keep these tiers explicit in release notes,
package names, and verification reports.

- Stable: platforms with real-device proof for launch, display, input, Spotify
  Connect visibility, local playback, and clean exit. Current stable packaging
  targets are NextUI, Stock, and CrossMix.
- Beta: platforms with at least one full real-device validation pass, but not
  enough repeated evidence to make them release blockers.
- Candidate: platforms where package layout and launcher scripts are believed
  correct, but there is no real-device proof yet. Candidate packages are for
  testers only and must not be described as supported.

KNULLI and muOS are Candidate-tier targets until device evidence promotes them.
Do not make KNULLI or muOS failures block Stable releases while they remain
Candidate-tier platforms.

Candidate package naming should make the status obvious:

- KNULLI: `SideB-<version>-knulli-candidate.zip`
- muOS: `SideB-<version>-muos-candidate.muxapp` or
  `SideB-<version>-muos-candidate.muxzip`, depending on the final Archive
  Manager-compatible format chosen during implementation.

Candidate artifacts must not break the Stable release gate. If Candidate
artifacts are produced by the main packager, validate Stable asset counts
separately; otherwise produce Candidate packages through an explicit
candidate-packaging command.

Candidate-to-Beta promotion requires real-device proof on the target OS:

- SideB launches from the native frontend.
- The framebuffer UI is visible and correctly framed.
- Hardware input works for core controls and exit.
- Spotify Connect advertises and can be selected from another client.
- Local playback uses the bundled `/tmp/ffmpeg-lite` decode path and produces
  audible output.
- Exiting returns cleanly to the OS frontend without stale display, stuck audio,
  or orphaned `sideb`/`go-librespot` processes.
- Logs and package hashes are captured with the report.

## KNULLI and muOS Candidate Strategy

KNULLI should be treated as a PortMaster/EmulationStation-style candidate first.
The likely package shape is a `roms/ports` launcher script plus a SideB payload
directory. Keep it as a thin wrapper around SideB's existing single-directory
runtime model. If targeting PortMaster catalogue conventions, include the
required metadata files such as `port.json`, `README.md`, screenshots,
`gameinfo.xml`, and license coverage.

muOS should be treated as a native Applications/Archive Manager candidate first.
Prefer a `mux_launch.sh`-based application package with muOS metadata and icon
assets. Do not hard-code SD-card mount paths; muOS storage can move between SD1
and SD2. Follow current muOS application runner conventions when implementing
the wrapper, including runner-provided app paths and environment setup.

For both Candidate platforms:

- Keep platform adaptation in packaging wrappers and validation scripts unless
  real-device evidence proves a core runtime change is necessary.
- Preserve the app-relative `data/` directory so favorites, cookies, imported
  music, and local cache can survive package updates.
- Treat `/dev/fb0`, `/dev/input`, SDL library discovery, ALSA/audio routing,
  `/dev/disp`, keepalive files, and CPU governor handling as runtime probes
  that need device evidence.
- Include a tester evidence script or checklist before publishing Candidate
  packages.
- If evidence is missing, report "Candidate only, not validated on device"
  rather than inferring support from successful local packaging.

## Bundled Audio Runtime

SideB should use the bundled `ffmpeg-lite` as the controlled audio
decode/transcode path. Do not make local playback depend on finding a system
`ffmpeg` in `PATH`; behavior needs to stay reproducible across NextUI, Stock,
CrossMix, and future TrimUI-style systems.

Runtime contract:

- Packaged binary: `package/SideB.pak/ffmpeg-lite`
- Device runtime path: `/tmp/ffmpeg-lite`
- Required capabilities: `libmp3lame` encoder for downloaded MP3 transcoding
  and `s16le` muxer for local playback PCM output consumed by SideB's playback
  backend
- Verification gate:
  `./scripts/check_ffmpeg_audio_transcoder.sh package/SideB.pak/ffmpeg-lite`

The system `/usr/bin/ffmpeg` path is still a best-effort helper for
embedded-cover extraction during import, not the playback path. Current local
playback decodes through bundled `/tmp/ffmpeg-lite` and outputs via SideB's SDL
audio callback; do not reintroduce system `aplay` as a required playback
dependency without device evidence and an updated fallback plan.

When changing the bundled FFmpeg build, update `packaging/ffmpeg/Dockerfile`,
rebuild via `./scripts/build_ffmpeg_audio_transcoder.sh`, refresh
`packaging/shared/LICENSES/THIRD_PARTY_SOURCES.md`, and measure package-size
impact before release.
