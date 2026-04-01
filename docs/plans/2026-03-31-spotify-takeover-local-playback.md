# Spotify Takeover Local Playback Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Stop local playback whenever Spotify resumes on another device and restore the local UI to the correct paused local track after Spotify stops.

**Architecture:** Spotify event handling emits takeover commands on both connection activation and playback-resume events. The command layer records the currently playing local URI before stopping local playback, then restores local UI state from downloaded favorites when Spotify deactivates, preferring the remembered URI and falling back to the first downloaded local track.

**Tech Stack:** Rust binary crate, command/event pipeline, unit tests in `main.rs` and `network.rs`.

---

### Task 1: Lock takeover detection with failing tests

**Files:**
- Modify: `spotify-ui-rs/src/network.rs`
- Test: `spotify-ui-rs/src/network.rs`

**Step 1: Write the failing test**

Add a test that proves a `playing` event while the app is in local mode dispatches `InputAction::SpotifyActivated`.

**Step 2: Run test to verify it fails**

Run: `cargo test spotify_playing_event_dispatches_takeover`
Expected: FAIL because `playing` currently does not dispatch takeover

**Step 3: Write minimal implementation**

Dispatch `SpotifyActivated` on `playing` and `will_play`.

**Step 4: Run test to verify it passes**

Run: `cargo test spotify_playing_event_dispatches_takeover`
Expected: PASS

### Task 2: Lock local restore selection with failing tests

**Files:**
- Modify: `spotify-ui-rs/src/main.rs`
- Modify: `spotify-ui-rs/src/app.rs`
- Test: `spotify-ui-rs/src/main.rs`

**Step 1: Write the failing test**

Add tests that prove:
- a remembered local URI is preferred when it still exists
- fallback uses the first downloaded track when the remembered URI is missing
- no downloaded tracks yields no restore target

**Step 2: Run test to verify it fails**

Run: `cargo test local_restore_target`
Expected: FAIL because the helper does not exist yet

**Step 3: Write minimal implementation**

Replace the `local_was_playing` flag with a remembered local URI and add a helper for selecting the restore target.

**Step 4: Run test to verify it passes**

Run: `cargo test local_restore_target`
Expected: PASS

### Task 3: Implement takeover and paused restore behavior

**Files:**
- Modify: `spotify-ui-rs/src/main.rs`
- Modify: `spotify-ui-rs/src/app.rs`
- Modify: `spotify-ui-rs/src/network.rs`

**Step 1: Keep the takeover and restore tests as the red-green contract**

Use the tests from Tasks 1 and 2 as the safety net while changing runtime logic.

**Step 2: Write minimal implementation**

Stop local playback on Spotify takeover, remember the local URI, and restore local paused UI state on Spotify deactivation using downloaded favorites.

**Step 3: Run targeted verification**

Run: `cargo test spotify_playing_event_dispatches_takeover local_restore_target`
Expected: PASS

### Task 4: Run full verification

**Files:**
- Modify: `spotify-ui-rs/src/main.rs`
- Modify: `spotify-ui-rs/src/network.rs`
- Modify: `spotify-ui-rs/src/app.rs`

**Step 1: Run the full Rust test suite**

Run: `cargo test`
Expected: all tests pass

**Step 2: Build the release package**

Run: `bash scripts/package.sh`
Expected: exit 0 and fresh package artifacts
