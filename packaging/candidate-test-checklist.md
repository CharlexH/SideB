# KNULLI and muOS Candidate Test Checklist

Use this checklist for KNULLI and muOS Candidate packages. Candidate means the
package layout and launcher are prepared, but the package is not supported until
real-device evidence proves the runtime path.

## Package Identity

- OS and version:
- Device model:
- SideB archive filename:
- SideB archive SHA256:
- Install path:
- Clean install or upgrade install:

## Required Runtime Evidence

Run these on the device after launching SideB once:

```sh
uname -a
cat /etc/os-release 2>/dev/null || true
ldd --version 2>&1 | head -n 1
ls -l /dev/fb0 /dev/input/event* 2>/dev/null
ls -l /tmp/sideb /tmp/go-librespot /tmp/ffmpeg-lite /tmp/yt-dlp 2>/dev/null
cat /tmp/sideb.log 2>/dev/null
cat /tmp/go-librespot.log 2>/dev/null
ps | grep -E 'sideb|go-librespot|ffmpeg-lite' | grep -v grep
```

## Manual Validation

- SideB launches from the native frontend.
- Framebuffer UI is visible and correctly framed.
- A / play-pause works.
- Left / right track navigation works.
- Up / down volume works.
- X favorite or remove confirmation works.
- Y opens and closes FAV LIST.
- B or MENU exits SideB.
- Spotify shows the SideB device on another client.
- Spotify Connect playback starts without crashing the UI.
- Local playback starts from FAV LIST.
- Local playback uses `/tmp/ffmpeg-lite` and produces audible output.
- Exit returns cleanly to the OS frontend.
- No stale framebuffer remains after exit.
- No orphaned `sideb`, `go-librespot`, or `ffmpeg-lite` process remains after exit.
- Upgrade install preserves `data/` contents, including favorites, cookies,
  imported tracks, and cached music.

## Promotion Rule

KNULLI and muOS are promoted independently. A platform can move from Candidate
to Beta only after a complete evidence packet proves launch, display, input,
Spotify Connect, local playback, clean exit, package/runtime hashes, and logs on
the same archive build.
