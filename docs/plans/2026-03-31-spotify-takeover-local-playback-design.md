# SideB Spotify Takeover Local Playback Design

**Date:** 2026-03-31

**Goal:** Ensure Spotify playback always preempts local playback, even when Spotify Connect is already active, and restore the local UI to a paused local-track state after Spotify stops.

## Scope

- Stop local playback when Spotify starts playing from another device.
- Detect takeover on `playing` and `will_play`, not just `active`.
- Remember the preempted local track URI.
- When Spotify stops:
  - return to `Waiting` if no local downloads exist
  - otherwise return to local mode, paused, showing the remembered local track if still available
  - fall back to the first downloaded local track if the remembered track no longer exists

## Constraints

- Local playback should be stopped, not paused.
- Returning to local mode should not auto-resume audio.
- Existing Spotify cover and metadata handling should keep working.

## Chosen Approach

Replace the current `local_was_playing` pause/resume model with a remembered local-track URI. Emit `SpotifyActivated` not only on `active`, but also on playback-resume events (`playing`, `will_play`) so takeover is detected even when Spotify Connect was already active before local playback started.

## Testing

- Add a network event test that `playing` while in local mode dispatches `SpotifyActivated`.
- Add pure helper tests for selecting the correct local track to restore after Spotify stops.
