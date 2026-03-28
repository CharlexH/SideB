use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};
use std::{fs, path::{Path, PathBuf}};

use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

use crate::app::AppState;
use crate::constants::API_BASE;
use crate::render::{CoverUpdate, RenderState};
use crate::resources;
use crate::types::*;

const STATUS_SYNC_INTERVAL: Duration = Duration::from_secs(5);
const STATUS_SYNC_BOOST_INTERVAL: Duration = Duration::from_millis(750);
const STATUS_SYNC_BOOST_DURATION: Duration = Duration::from_secs(3);
const STATUS_SYNC_ENDGAME_INTERVAL: Duration = Duration::from_secs(1);
const STATUS_SYNC_ENDGAME_THRESHOLD_MS: i64 = 10_000;
const POSITION_CORRECTION_THRESHOLD_MS: i64 = 800;
const BOOST_POSITION_CORRECTION_THRESHOLD_MS: i64 = 300;
const STATUS_SYNC_IDLE_SLEEP: Duration = Duration::from_millis(250);

/// POST to a go-librespot API endpoint.
pub fn api_post(path: &str) {
    let url = format!("{API_BASE}{path}");
    match ureq::post(&url).send_empty() {
        Ok(_) => {}
        Err(e) => eprintln!("api error: {e}"),
    }
}

/// POST volume change with JSON body.
pub fn api_post_volume(delta: i32) {
    let url = format!("{API_BASE}/player/volume");
    let body = format!(r#"{{"volume":{delta},"relative":true}}"#);
    match ureq::post(&url)
        .header("Content-Type", "application/json")
        .send(body.as_bytes())
    {
        Ok(_) => {}
        Err(e) => eprintln!("api error: {e}"),
    }
}

fn next_status_sync_interval(
    connected: bool,
    paused: bool,
    duration_ms: i64,
    position_ms: i64,
    now: Instant,
    boost_until: Instant,
) -> Option<Duration> {
    if !connected || paused {
        return None;
    }

    if now < boost_until {
        return Some(STATUS_SYNC_BOOST_INTERVAL);
    }

    if duration_ms > 0 && duration_ms.saturating_sub(position_ms) <= STATUS_SYNC_ENDGAME_THRESHOLD_MS {
        return Some(STATUS_SYNC_ENDGAME_INTERVAL);
    }

    Some(STATUS_SYNC_INTERVAL)
}

fn should_apply_position_correction(
    current_ms: i64,
    authoritative_ms: i64,
    threshold_ms: i64,
) -> bool {
    current_ms.abs_diff(authoritative_ms) >= threshold_ms as u64
}

fn estimated_position_ms(state: &AppState, now: Instant) -> i64 {
    let mut position = state.position.max(0);
    if state.connected && !state.paused && state.duration > 0 {
        position += now
            .saturating_duration_since(state.last_pos_time)
            .as_millis() as i64;
        position = position.min(state.duration);
    }
    position
}

fn position_correction_threshold(now: Instant, boost_until: Instant) -> i64 {
    if now < boost_until {
        BOOST_POSITION_CORRECTION_THRESHOLD_MS
    } else {
        POSITION_CORRECTION_THRESHOLD_MS
    }
}

fn mark_status_sync_boost(state: &Arc<Mutex<AppState>>, now: Instant) {
    state
        .lock()
        .unwrap()
        .boost_status_sync(now, STATUS_SYNC_BOOST_DURATION);
}

fn apply_track_snapshot(
    state: &mut AppState,
    track: &Track,
    paused: bool,
    volume: i32,
    volume_steps: i32,
    position_threshold_ms: Option<i64>,
    now: Instant,
) {
    let track_changed = state.current_track_uri != track.uri;
    if track_changed {
        state.current_track_uri = track.uri.clone();
    }

    if state.track_name != track.name {
        state.track_name = track.name.clone();
    }
    let artist_name = track.artist_names.join(", ");
    if state.artist_name != artist_name {
        state.artist_name = artist_name;
    }
    if state.album_name != track.album_name {
        state.album_name = track.album_name.clone();
    }

    state.set_duration(track.duration);
    state.set_connected(true);
    state.set_paused(paused);
    state.set_volume(volume, volume_steps);

    let should_sync_position = match position_threshold_ms {
        None => true,
        Some(_threshold_ms) if paused || track_changed => true,
        Some(threshold_ms) => {
            let current_position = estimated_position_ms(state, now);
            should_apply_position_correction(current_position, track.position, threshold_ms)
        }
    };

    if should_sync_position {
        if let Some(threshold_ms) = position_threshold_ms {
            let current_position = estimated_position_ms(state, now);
            eprintln!(
                "status sync corrected position {} -> {} ms (threshold {} ms)",
                current_position,
                track.position,
                threshold_ms
            );
        }
        state.set_position(track.position, now);
    }
}

/// Fetch player status from go-librespot API.
fn fetch_status(
    state: &Arc<Mutex<AppState>>,
    render_state: &Arc<Mutex<RenderState>>,
    position_threshold_ms: Option<i64>,
) {
    let url = format!("{API_BASE}/status");
    let body = match ureq::get(&url).call() {
        Ok(resp) => match resp.into_body().read_to_string() {
            Ok(s) => s,
            Err(_) => return,
        },
        Err(_) => return,
    };

    let status: PlayerStatus = match serde_json::from_str(&body) {
        Ok(s) => s,
        Err(_) => return,
    };

    let now = Instant::now();

    // Don't let status polling overwrite UI state during local playback
    if state.lock().unwrap().mode == crate::mode::AppMode::Local {
        return;
    }

    if let Some(track) = &status.track {
        let cover_url = prefer_high_res_cover_url(&track.album_cover_url);
        {
            let mut st = state.lock().unwrap();
            apply_track_snapshot(
                &mut st,
                track,
                status.paused,
                status.volume,
                status.volume_steps,
                position_threshold_ms,
                now,
            );
        }
        update_cover(Some(&cover_url), render_state);
    } else {
        let mut st = state.lock().unwrap();
        st.set_connected(!status.username.is_empty());
        st.current_track_uri.clear();
    }
}

fn cover_log_key(url: &str) -> String {
    cover_cache_key(url).chars().take(8).collect()
}

fn prefer_high_res_cover_url(url: &str) -> String {
    if !url.starts_with("https://i.scdn.co/image/") {
        return url.to_string();
    }

    url.replace("ab67616d00004851", "ab67616d0000b273")
        .replace("ab67616d00001e02", "ab67616d0000b273")
}

fn lock_render_state_for_update<'a>(
    render_state: &'a Arc<Mutex<RenderState>>,
) -> MutexGuard<'a, RenderState> {
    let started = Instant::now();

    loop {
        match render_state.try_lock() {
            Ok(guard) => {
                let waited = started.elapsed().as_millis();
                if waited > 10 {
                    eprintln!("render-state lock waited {} ms", waited);
                }
                return guard;
            }
            Err(TryLockError::Poisoned(err)) => return err.into_inner(),
            Err(TryLockError::WouldBlock) => std::thread::yield_now(),
        }
    }
}

fn cover_fetch_curl_args<'a>(cert_file: &'a str, url: &'a str) -> Vec<&'a str> {
    vec![
        "-4",
        "-fsSL",
        "--connect-timeout",
        "3",
        "--max-time",
        "10",
        "--cacert",
        cert_file,
        url,
    ]
}

fn cover_cache_root() -> PathBuf {
    PathBuf::from("/tmp/spotify-ui-cover-cache")
}

fn cover_cache_key(url: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for &byte in url.as_bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}.img")
}

fn cover_cache_path(cache_root: &Path, url: &str) -> PathBuf {
    cache_root.join(cover_cache_key(url))
}

fn read_cover_cache(cache_root: &Path, url: &str) -> Option<Vec<u8>> {
    let cache_path = cover_cache_path(cache_root, url);
    match fs::read(&cache_path) {
        Ok(bytes) if !bytes.is_empty() => {
            eprintln!(
                "cover {} cache hit: {}",
                cover_log_key(url),
                cache_path.display()
            );
            Some(bytes)
        }
        _ => None,
    }
}

fn fetch_cover_bytes_with<F>(url: &str, cache_root: &Path, fetcher: F) -> Option<Vec<u8>>
where
    F: FnOnce(&str) -> Option<Vec<u8>>,
{
    if let Some(bytes) = read_cover_cache(cache_root, url) {
        return Some(bytes);
    }

    let cache_path = cover_cache_path(cache_root, url);
    let bytes = fetcher(url)?;
    if bytes.is_empty() {
        return None;
    }

    if let Some(parent) = cache_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&cache_path, &bytes);
    Some(bytes)
}

fn load_cached_cover_from(
    url: &str,
    cache_root: &Path,
) -> Option<RgbaImage> {
    let bytes = match read_cover_cache(cache_root, url) {
        Some(bytes) => bytes,
        None => return None,
    };

    resources::decode_image_bytes(&bytes)
}

fn apply_cached_cover_if_present_from(
    url: &str,
    cache_root: &Path,
    render_state: &Arc<Mutex<RenderState>>,
) -> bool {
    let started = Instant::now();
    let img = match load_cached_cover_from(url, cache_root) {
        Some(img) => img,
        None => return false,
    };

    let mut rs = lock_render_state_for_update(render_state);
    let applied = rs.apply_cover_if_current(url, &img);
    eprintln!(
        "cover {} applied from cache in {} ms{}",
        cover_log_key(url),
        started.elapsed().as_millis(),
        if applied { "" } else { " (stale)" }
    );
    applied
}

fn fetch_cover_bytes(url: &str) -> Option<Vec<u8>> {
    fetch_cover_bytes_with(url, &cover_cache_root(), |url| {
        let started = Instant::now();
        eprintln!("cover {} fetch start", cover_log_key(url));
        if url.starts_with("https://") {
            let cert_file = std::env::var("SSL_CERT_FILE")
                .unwrap_or_else(|_| "resources/ca-certificates.crt".to_string());
            match std::process::Command::new("curl")
                .args(cover_fetch_curl_args(&cert_file, url))
                .output()
            {
                Ok(out) if out.status.success() && !out.stdout.is_empty() => {
                    eprintln!(
                        "cover {} fetch done in {} ms ({} bytes)",
                        cover_log_key(url),
                        started.elapsed().as_millis(),
                        out.stdout.len()
                    );
                    Some(out.stdout)
                }
                Ok(out) => {
                    eprintln!(
                        "cover {} curl failed: {}",
                        cover_log_key(url),
                        String::from_utf8_lossy(&out.stderr)
                    );
                    None
                }
                Err(e) => {
                    eprintln!("cover {} curl error: {e}", cover_log_key(url));
                    None
                }
            }
        } else {
            match ureq::get(url).call() {
                Ok(resp) => match resp.into_body().read_to_vec() {
                    Ok(v) if !v.is_empty() => {
                        eprintln!(
                            "cover {} fetch done in {} ms ({} bytes)",
                            cover_log_key(url),
                            started.elapsed().as_millis(),
                            v.len()
                        );
                        Some(v)
                    }
                    Ok(_) => {
                        eprintln!("cover {} fetch returned empty body", cover_log_key(url));
                        None
                    }
                    Err(e) => {
                        eprintln!("cover {} body read error: {e}", cover_log_key(url));
                        None
                    }
                },
                Err(e) => {
                    eprintln!("cover {} fetch error: {e}", cover_log_key(url));
                    None
                }
            }
        }
    })
}

fn spawn_cover_fetch(url: String, render_state: Arc<Mutex<RenderState>>) {
    std::thread::spawn(move || {
        let data = match fetch_cover_bytes(&url) {
            Some(data) => data,
            None => return,
        };

        let decode_started = Instant::now();
        let img = match resources::decode_image_bytes(&data) {
            Some(i) => i,
            None => {
                eprintln!("cover {} decode error", cover_log_key(&url));
                return;
            }
        };
        eprintln!(
            "cover {} decoded in {} ms",
            cover_log_key(&url),
            decode_started.elapsed().as_millis()
        );

        let mut rs = lock_render_state_for_update(&render_state);
        let applied = rs.apply_cover_if_current(&url, &img);
        eprintln!(
            "cover {} applied after fetch{}",
            cover_log_key(&url),
            if applied { "" } else { " (stale)" }
        );
    });
}

pub fn update_cover(cover_url: Option<&str>, render_state: &Arc<Mutex<RenderState>>) {
    let cache_root = cover_cache_root();

    if let Some(url) = cover_url.filter(|url| !url.is_empty()) {
        if let Some(img) = load_cached_cover_from(url, &cache_root) {
            let started = Instant::now();
            let mut rs = lock_render_state_for_update(render_state);

            if rs.applied_cover_url.as_deref() == Some(url) && rs.scene_cover.is_some() {
                return;
            }

            rs.replace_cover(url, &img);
            eprintln!(
                "cover {} swapped from cache in {} ms",
                cover_log_key(url),
                started.elapsed().as_millis()
            );
            return;
        }
    }

    let action = {
        let mut rs = lock_render_state_for_update(render_state);
        rs.plan_cover_update(cover_url)
    };

    if let CoverUpdate::Fetch(url) = action {
        spawn_cover_fetch(url, Arc::clone(render_state));
    }
}

/// WebSocket event listener — reconnects on disconnect.
pub fn listen_events(
    state: Arc<Mutex<AppState>>,
    render_state: Arc<Mutex<RenderState>>,
    quit: Arc<AtomicBool>,
    cmd_tx: std::sync::mpsc::Sender<crate::mode::InputAction>,
) {
    loop {
        if quit.load(Ordering::Relaxed) {
            return;
        }
        connect_websocket(&state, &render_state, &quit, &cmd_tx);
        std::thread::sleep(Duration::from_secs(2));
    }
}

pub fn poll_status(
    state: Arc<Mutex<AppState>>,
    render_state: Arc<Mutex<RenderState>>,
    quit: Arc<AtomicBool>,
) {
    let mut last_sync_at = Instant::now();

    loop {
        if quit.load(Ordering::Relaxed) {
            return;
        }

        let now = Instant::now();
        let interval = {
            let st = state.lock().unwrap();
            let position_ms = estimated_position_ms(&st, now);
            next_status_sync_interval(
                st.connected,
                st.paused,
                st.duration,
                position_ms,
                now,
                st.status_sync_boost_until,
            )
        };

        if let Some(interval) = interval {
            let due_at = last_sync_at + interval;
            if now >= due_at {
                let threshold_ms = {
                    let st = state.lock().unwrap();
                    position_correction_threshold(Instant::now(), st.status_sync_boost_until)
                };
                fetch_status(&state, &render_state, Some(threshold_ms));
                last_sync_at = Instant::now();
                continue;
            }

            let sleep_for = due_at
                .saturating_duration_since(now)
                .min(STATUS_SYNC_IDLE_SLEEP);
            std::thread::sleep(sleep_for);
        } else {
            std::thread::sleep(STATUS_SYNC_IDLE_SLEEP);
            last_sync_at = Instant::now();
        }

        if quit.load(Ordering::Relaxed) {
            return;
        }
    }
}

fn connect_websocket(
    state: &Arc<Mutex<AppState>>,
    render_state: &Arc<Mutex<RenderState>>,
    quit: &Arc<AtomicBool>,
    cmd_tx: &std::sync::mpsc::Sender<crate::mode::InputAction>,
) {
    fetch_status(state, render_state, None);

    let ws_url = "ws://127.0.0.1:3678/events";
    let (mut socket, _): (WebSocket<MaybeTlsStream<TcpStream>>, _) = match connect(ws_url) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ws connect error: {e}");
            return;
        }
    };

    eprintln!("websocket connected");

    loop {
        if quit.load(Ordering::Relaxed) {
            return;
        }

        let msg = match socket.read() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("ws read error: {e}");
                return;
            }
        };

        if let Message::Text(text) = msg {
            let ev: WSEvent = match serde_json::from_str(&text) {
                Ok(e) => e,
                Err(_) => continue,
            };
            handle_event(ev, state, render_state, cmd_tx);
        }
    }
}

fn handle_event(
    ev: WSEvent,
    state: &Arc<Mutex<AppState>>,
    render_state: &Arc<Mutex<RenderState>>,
    cmd_tx: &std::sync::mpsc::Sender<crate::mode::InputAction>,
) {
    eprintln!("event: {}", ev.event_type);

    match ev.event_type.as_str() {
        "metadata" => {
            if let Some(ref data) = ev.data {
                if let Ok(meta) = serde_json::from_str::<MetadataEvent>(data.get()) {
                    mark_status_sync_boost(state, Instant::now());
                    let cover_url = prefer_high_res_cover_url(&meta.album_cover_url);
                    let track_changed = {
                        let mut st = state.lock().unwrap();
                        let changed = st.current_track_uri != meta.uri;
                        st.current_track_uri = meta.uri;
                        st.track_name = meta.name;
                        st.artist_name = meta.artist_names.join(", ");
                        st.album_name = meta.album_name;
                        st.set_duration(meta.duration);
                        st.set_position(meta.position, Instant::now());
                        st.set_connected(true);
                        changed
                    };
                    update_cover(Some(&cover_url), render_state);
                    if track_changed {
                        let _ = cmd_tx.send(crate::mode::InputAction::SpotifyTrackChanged);
                    }
                }
            }
        }

        "playing" | "will_play" => {
            mark_status_sync_boost(state, Instant::now());
            let mut st = state.lock().unwrap();
            if st.mode != crate::mode::AppMode::Local {
                st.set_paused(false);
                st.last_pos_time = Instant::now();
            }
        }

        "paused" | "not_playing" | "will_pause" => {
            let mut st = state.lock().unwrap();
            if st.mode != crate::mode::AppMode::Local {
                st.set_paused(true);
            }
        }

        "stopped" => {
            let is_local = state.lock().unwrap().mode == crate::mode::AppMode::Local;
            if !is_local {
                let mut st = state.lock().unwrap();
                st.set_paused(true);
                st.track_name.clear();
                st.artist_name.clear();
                st.set_connected(false);
                drop(st);
                update_cover(None, render_state);
            }
            let _ = cmd_tx.send(crate::mode::InputAction::SpotifyDeactivated);
        }

        "volume" => {
            if let Some(ref data) = ev.data {
                if let Ok(vol) = serde_json::from_str::<VolumeEvent>(data.get()) {
                    let mut st = state.lock().unwrap();
                    st.set_volume(vol.value, vol.max);
                }
            }
        }

        "seek" => {
            if let Some(ref data) = ev.data {
                if let Ok(meta) = serde_json::from_str::<MetadataEvent>(data.get()) {
                    mark_status_sync_boost(state, Instant::now());
                    let mut st = state.lock().unwrap();
                    st.set_position(meta.position, Instant::now());
                }
            }
        }

        "active" => {
            mark_status_sync_boost(state, Instant::now());
            {
                let mut st = state.lock().unwrap();
                st.set_connected(true);
            }
            fetch_status(state, render_state, None);
            let _ = cmd_tx.send(crate::mode::InputAction::SpotifyActivated);
        }

        "inactive" => {
            let is_local = state.lock().unwrap().mode == crate::mode::AppMode::Local;
            if !is_local {
                let mut st = state.lock().unwrap();
                st.set_connected(false);
                st.current_track_uri.clear();
                st.track_name.clear();
                st.artist_name.clear();
                drop(st);
                update_cover(None, render_state);
            }
            let _ = cmd_tx.send(crate::mode::InputAction::SpotifyDeactivated);
        }

        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn empty_render_state() -> Arc<Mutex<RenderState>> {
        Arc::new(Mutex::new(RenderState {
            scene_base: vec![0u8; crate::constants::FB_SIZE],
            scene_playing: vec![0u8; crate::constants::FB_SIZE],
            scene_waiting: vec![0u8; crate::constants::FB_SIZE],
            scene_foreground: None,
            scene_cover: None,
            wheel_frames: Vec::new(),
            taperoll_cache: HashMap::new(),
            full_redraw: false,
            cover_mask: None,
            img_playing: None,
            img_paused: None,
            requested_cover_url: None,
            applied_cover_url: None,
        }))
    }

    fn make_event(event_type: &str, data: Option<&str>) -> WSEvent {
        WSEvent {
            event_type: event_type.to_string(),
            data: data.map(|json| serde_json::value::RawValue::from_string(json.to_string()).unwrap()),
        }
    }

    #[test]
    fn repeated_paused_event_does_not_mark_dirty() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let render_state = empty_render_state();
        {
            let mut st = state.lock().unwrap();
            st.paused = true;
            st.connected = true;
            st.render_dirty = false;
        }

        handle_event(make_event("paused", None), &state, &render_state);

        let st = state.lock().unwrap();
        assert!(st.paused);
        assert!(!st.render_dirty);
    }

    #[test]
    fn will_pause_event_switches_to_paused_state() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let render_state = empty_render_state();
        {
            let mut st = state.lock().unwrap();
            st.paused = false;
            st.render_dirty = false;
        }

        handle_event(make_event("will_pause", None), &state, &render_state);

        let st = state.lock().unwrap();
        assert!(st.paused);
        assert!(st.render_dirty);
    }

    #[test]
    fn unchanged_volume_event_does_not_mark_dirty() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let render_state = empty_render_state();
        {
            let mut st = state.lock().unwrap();
            st.volume = 80;
            st.volume_max = 100;
            st.render_dirty = false;
        }

        handle_event(
            make_event("volume", Some(r#"{"value":80,"max":100}"#)),
            &state,
            &render_state,
        );

        let st = state.lock().unwrap();
        assert_eq!(st.volume, 80);
        assert_eq!(st.volume_max, 100);
        assert!(!st.render_dirty);
    }

    #[test]
    fn changed_volume_event_marks_dirty() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let render_state = empty_render_state();
        {
            let mut st = state.lock().unwrap();
            st.volume = 80;
            st.volume_max = 100;
            st.render_dirty = false;
        }

        handle_event(
            make_event("volume", Some(r#"{"value":75,"max":100}"#)),
            &state,
            &render_state,
        );

        let st = state.lock().unwrap();
        assert_eq!(st.volume, 75);
        assert!(st.render_dirty);
    }

    #[test]
    fn https_cover_fetch_uses_ipv4_and_timeouts() {
        let args = cover_fetch_curl_args(
            "resources/ca-certificates.crt",
            "https://i.scdn.co/image/example",
        );

        assert_eq!(
            args,
            vec![
                "-4",
                "-fsSL",
                "--connect-timeout",
                "3",
                "--max-time",
                "10",
                "--cacert",
                "resources/ca-certificates.crt",
                "https://i.scdn.co/image/example",
            ]
        );
    }

    fn unique_cache_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("spotify-ui-cover-cache-test-{nanos}"))
    }

    fn write_test_png(path: &Path) {
        let file = fs::File::create(path).unwrap();
        let mut encoder = png::Encoder::new(file, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[255, 0, 0, 255]).unwrap();
    }

    #[test]
    fn cover_fetch_uses_disk_cache_after_first_fetch() {
        let cache_dir = unique_cache_dir();
        let calls = Cell::new(0);
        let url = "https://i.scdn.co/image/cache-test";
        let expected = vec![1u8, 2, 3, 4];

        let first = fetch_cover_bytes_with(url, &cache_dir, |requested| {
            assert_eq!(requested, url);
            calls.set(calls.get() + 1);
            Some(expected.clone())
        });
        let second = fetch_cover_bytes_with(url, &cache_dir, |_| {
            calls.set(calls.get() + 1);
            Some(vec![9u8, 9, 9])
        });

        assert_eq!(first, Some(expected.clone()));
        assert_eq!(second, Some(expected));
        assert_eq!(calls.get(), 1);

        let _ = fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn cached_cover_is_applied_synchronously() {
        let cache_dir = unique_cache_dir();
        fs::create_dir_all(&cache_dir).unwrap();
        let url = "https://i.scdn.co/image/cached-cover";
        let cache_path = cover_cache_path(&cache_dir, url);
        write_test_png(&cache_path);

        let render_state = empty_render_state();
        {
            let mut rs = render_state.lock().unwrap();
            assert_eq!(
                rs.plan_cover_update(Some(url)),
                CoverUpdate::Fetch(url.to_string())
            );
        }

        assert!(apply_cached_cover_if_present_from(url, &cache_dir, &render_state));

        let rs = render_state.lock().unwrap();
        assert_eq!(rs.applied_cover_url.as_deref(), Some(url));
        assert!(rs.scene_cover.is_some());

        let _ = fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn cover_log_key_uses_stable_short_hash_prefix() {
        assert_eq!(cover_log_key("https://i.scdn.co/image/cached-cover").len(), 8);
        assert_eq!(
            cover_log_key("https://i.scdn.co/image/cached-cover"),
            cover_log_key("https://i.scdn.co/image/cached-cover")
        );
    }

    #[test]
    fn spotify_cover_urls_are_upgraded_to_640_square() {
        assert_eq!(
            prefer_high_res_cover_url("https://i.scdn.co/image/ab67616d00001e0254b26107b2b819ad77e17311"),
            "https://i.scdn.co/image/ab67616d0000b27354b26107b2b819ad77e17311"
        );
        assert_eq!(
            prefer_high_res_cover_url("https://i.scdn.co/image/ab67616d0000485154b26107b2b819ad77e17311"),
            "https://i.scdn.co/image/ab67616d0000b27354b26107b2b819ad77e17311"
        );
    }

    #[test]
    fn non_spotify_cover_urls_are_left_unchanged() {
        let url = "https://example.com/image/ab67616d00001e02foo";
        assert_eq!(prefer_high_res_cover_url(url), url);
    }

    #[test]
    fn status_sync_interval_defaults_to_five_seconds_while_playing() {
        let now = Instant::now();
        assert_eq!(
            next_status_sync_interval(true, false, 120_000, 30_000, now, now),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn status_sync_interval_is_faster_during_boost_window() {
        let now = Instant::now();
        assert_eq!(
            next_status_sync_interval(
                true,
                false,
                120_000,
                30_000,
                now,
                now + Duration::from_secs(3)
            ),
            Some(Duration::from_millis(750))
        );
    }

    #[test]
    fn status_sync_interval_is_faster_near_track_end() {
        let now = Instant::now();
        assert_eq!(
            next_status_sync_interval(true, false, 120_000, 111_500, now, now),
            Some(Duration::from_secs(1))
        );
    }

    #[test]
    fn position_correction_ignores_small_drift() {
        assert!(!should_apply_position_correction(10_000, 10_500, 800));
        assert!(should_apply_position_correction(10_000, 11_200, 800));
    }
}
