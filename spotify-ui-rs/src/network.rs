use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use std::{
    fs,
    path::{Path, PathBuf},
};

use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

use crate::app::AppState;
use crate::constants::API_BASE;
use crate::mode::InputAction;
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
const STATUS_SYNC_BUSY_BACKOFF: Duration = Duration::from_secs(3);
const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const API_COMMAND_TIMEOUT: Duration = Duration::from_secs(25);
const SPOTIFY_EVENT_PENDING_WINDOW: Duration = Duration::from_secs(12);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiCommandResult {
    Ok,
    Busy,
    Offline,
}

impl ApiCommandResult {
    fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

fn api_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(API_REQUEST_TIMEOUT))
            .build()
            .into()
    })
}

fn api_command_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(API_COMMAND_TIMEOUT))
            .build()
            .into()
    })
}

/// POST to a go-librespot API endpoint.
pub fn api_post(path: &str) -> bool {
    api_post_result(path).is_ok()
}

pub fn api_post_result(path: &str) -> ApiCommandResult {
    let url = format!("{API_BASE}{path}");
    api_post_url_result(&url)
}

pub fn api_post_async(path: &'static str) {
    let spawn_result = std::thread::Builder::new()
        .name("spotify-api".into())
        .spawn(move || {
            let _ = api_post_result(path);
        });
    if let Err(error) = spawn_result {
        eprintln!("api command spawn failed path={path}: {error}");
    }
}

fn api_post_url(url: &str) -> bool {
    api_post_url_result(url).is_ok()
}

fn api_post_url_result(url: &str) -> ApiCommandResult {
    let started = Instant::now();
    let result = match api_command_agent().post(url).send_empty() {
        Ok(_) => ApiCommandResult::Ok,
        Err(e) => {
            log_api_command_error(&e);
            classify_api_command_error(&e)
        }
    };
    log_api_command_timing("POST", url, result, started.elapsed());
    result
}

/// POST volume change with JSON body.
pub fn api_post_volume(delta: i32) -> bool {
    api_post_volume_result(delta).is_ok()
}

pub fn api_post_volume_result(delta: i32) -> ApiCommandResult {
    let url = format!("{API_BASE}/player/volume");
    let body = format!(r#"{{"volume":{delta},"relative":true}}"#);
    let started = Instant::now();
    let result = match api_command_agent()
        .post(&url)
        .header("Content-Type", "application/json")
        .send(body.as_bytes())
    {
        Ok(_) => ApiCommandResult::Ok,
        Err(e) => {
            log_api_command_error(&e);
            classify_api_command_error(&e)
        }
    };
    log_api_command_timing("POST", &url, result, started.elapsed());
    result
}

pub fn api_post_volume_async(delta: i32) {
    let spawn_result = std::thread::Builder::new()
        .name("spotify-api".into())
        .spawn(move || {
            let _ = api_post_volume_result(delta);
        });
    if let Err(error) = spawn_result {
        eprintln!("api command spawn failed path=/player/volume delta={delta}: {error}");
    }
}

fn classify_api_command_error(error: &ureq::Error) -> ApiCommandResult {
    match error {
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => ApiCommandResult::Offline,
        ureq::Error::Io(err) if is_offline_io_error(err.kind()) => ApiCommandResult::Offline,
        _ => ApiCommandResult::Busy,
    }
}

fn is_offline_io_error(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::AddrNotAvailable
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::UnexpectedEof
    )
}

fn log_api_command_error(error: &ureq::Error) {
    match classify_api_command_error(error) {
        ApiCommandResult::Offline => eprintln!("api error: {error}"),
        ApiCommandResult::Busy => eprintln!("api busy: {error}"),
        ApiCommandResult::Ok => {}
    }
}

fn log_api_command_timing(method: &str, url: &str, result: ApiCommandResult, elapsed: Duration) {
    if result != ApiCommandResult::Ok || elapsed >= Duration::from_millis(250) {
        eprintln!(
            "api command method={} path={} result={:?} elapsed_ms={}",
            method,
            url.strip_prefix(API_BASE).unwrap_or(url),
            result,
            elapsed.as_millis()
        );
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

    if duration_ms > 0
        && duration_ms.saturating_sub(position_ms) <= STATUS_SYNC_ENDGAME_THRESHOLD_MS
    {
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
    state.clear_spotify_command_pending();
    state.spotify_was_active = true;
    state.set_stop_to_sleep_eligible(false);

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
                current_position, track.position, threshold_ms
            );
        }
        state.set_position(track.position, now);
    }
}

fn clear_spotify_playback_snapshot(state: &mut AppState, now: Instant) {
    state.set_connected(false);
    state.set_paused(true);
    state.current_track_uri.clear();
    state.track_name.clear();
    state.artist_name.clear();
    state.album_name.clear();
    state.set_duration(0);
    state.set_position(0, now);
    state.set_favorited(false);
    state.clear_spotify_command_pending();
    state.set_stop_to_sleep_eligible(false);
}

fn mark_backend_unavailable(state: &Arc<Mutex<AppState>>) {
    let mut st = state.lock().unwrap();
    if st.mode == crate::mode::AppMode::Local {
        return;
    }
    st.set_connected(false);
    st.show_notice("Spotify offline", Instant::now());
}

fn handle_status_error(error: &ureq::Error, state: &Arc<Mutex<AppState>>) -> ApiCommandResult {
    let result = classify_api_command_error(error);
    match result {
        ApiCommandResult::Offline => mark_backend_unavailable(state),
        ApiCommandResult::Busy => eprintln!("status sync delayed: {error}"),
        ApiCommandResult::Ok => {}
    }
    result
}

fn status_sync_backoff_until(result: ApiCommandResult, now: Instant) -> Option<Instant> {
    match result {
        ApiCommandResult::Busy => Some(now + STATUS_SYNC_BUSY_BACKOFF),
        ApiCommandResult::Ok | ApiCommandResult::Offline => None,
    }
}

fn spotify_command_pending_sleep_for(state: &AppState, now: Instant) -> Option<Duration> {
    state
        .spotify_command_pending_until
        .and_then(|pending_until| pending_until.checked_duration_since(now))
        .filter(|duration| !duration.is_zero())
}

/// Fetch player status from go-librespot API.
fn fetch_status(
    state: &Arc<Mutex<AppState>>,
    render_state: &Arc<Mutex<RenderState>>,
    position_threshold_ms: Option<i64>,
    cmd_tx: Option<&std::sync::mpsc::Sender<InputAction>>,
) -> ApiCommandResult {
    let url = format!("{API_BASE}/status");
    let body = match api_agent().get(url.as_str()).call() {
        Ok(resp) => {
            if resp.status().as_u16() == 204 {
                handle_status_body("", state, render_state, position_threshold_ms, cmd_tx);
                return ApiCommandResult::Ok;
            }
            match resp.into_body().read_to_string() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("status body read failed: {e}");
                    mark_backend_unavailable(state);
                    return ApiCommandResult::Offline;
                }
            }
        }
        Err(e) => {
            return handle_status_error(&e, state);
        }
    };

    handle_status_body(&body, state, render_state, position_threshold_ms, cmd_tx);
    ApiCommandResult::Ok
}

fn handle_inactive_spotify_status(
    state: &Arc<Mutex<AppState>>,
    render_state: &Arc<Mutex<RenderState>>,
    cmd_tx: Option<&std::sync::mpsc::Sender<InputAction>>,
    now: Instant,
) {
    if state.lock().unwrap().mode == crate::mode::AppMode::Local {
        return;
    }
    {
        let mut st = state.lock().unwrap();
        clear_spotify_playback_snapshot(&mut st, now);
    }
    update_cover(None, render_state);
    if let Some(tx) = cmd_tx {
        let _ = tx.send(InputAction::SpotifyDeactivated);
    }
}

fn handle_status_body(
    body: &str,
    state: &Arc<Mutex<AppState>>,
    render_state: &Arc<Mutex<RenderState>>,
    position_threshold_ms: Option<i64>,
    cmd_tx: Option<&std::sync::mpsc::Sender<InputAction>>,
) {
    let now = Instant::now();

    if body.trim().is_empty() {
        handle_inactive_spotify_status(state, render_state, cmd_tx, now);
        return;
    }

    let status: PlayerStatus = match serde_json::from_str(body) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("status parse failed: {e}");
            mark_backend_unavailable(state);
            return;
        }
    };

    // Don't let status polling overwrite UI state during local playback
    if state.lock().unwrap().mode == crate::mode::AppMode::Local {
        return;
    }

    if status.stopped {
        handle_inactive_spotify_status(state, render_state, cmd_tx, now);
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
        st.set_paused(true);
        st.current_track_uri.clear();
        st.track_name.clear();
        st.artist_name.clear();
        st.album_name.clear();
        st.set_duration(0);
        st.set_position(0, now);
        st.clear_spotify_command_pending();
        st.set_stop_to_sleep_eligible(false);
        drop(st);
        update_cover(None, render_state);
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

pub(crate) fn is_allowed_cover_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let Some((host, path)) = rest.split_once('/') else {
        return false;
    };
    let host = host.to_ascii_lowercase();

    if host == "i.scdn.co" {
        return path.starts_with("image/");
    }

    host == "a1.mzstatic.com" || host.ends_with(".mzstatic.com")
}

fn lock_render_state_for_update<'a>(
    render_state: &'a Arc<Mutex<RenderState>>,
) -> MutexGuard<'a, RenderState> {
    match render_state.lock() {
        Ok(guard) => guard,
        Err(err) => err.into_inner(),
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
    PathBuf::from("/tmp/sideb-cover-cache")
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
    if !is_allowed_cover_url(url) {
        eprintln!("cover {} rejected by cover URL policy", cover_log_key(url));
        return None;
    }

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

fn load_cached_cover_from(url: &str, cache_root: &Path) -> Option<RgbaImage> {
    let bytes = read_cover_cache(cache_root, url)?;

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
    cmd_tx: std::sync::mpsc::Sender<InputAction>,
) {
    let mut last_sync_at = Instant::now();
    let mut busy_backoff_until: Option<Instant> = None;

    loop {
        if quit.load(Ordering::Relaxed) {
            return;
        }

        let now = Instant::now();
        if let Some(backoff_until) = busy_backoff_until {
            if now < backoff_until {
                std::thread::sleep(
                    backoff_until
                        .saturating_duration_since(now)
                        .min(STATUS_SYNC_IDLE_SLEEP),
                );
                continue;
            }
            busy_backoff_until = None;
        }

        let pending_sleep = {
            let st = state.lock().unwrap();
            spotify_command_pending_sleep_for(&st, now)
        };
        if let Some(sleep_for) = pending_sleep {
            std::thread::sleep(sleep_for.min(STATUS_SYNC_IDLE_SLEEP));
            continue;
        }

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
                let result = fetch_status(&state, &render_state, Some(threshold_ms), Some(&cmd_tx));
                busy_backoff_until = status_sync_backoff_until(result, Instant::now());
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
    fetch_status(state, render_state, None, Some(cmd_tx));

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
                        st.clear_spotify_command_pending();
                        st.spotify_was_active = true;
                        st.set_stop_to_sleep_eligible(false);
                        changed
                    };
                    update_cover(Some(&cover_url), render_state);
                    if track_changed {
                        let _ = cmd_tx.send(crate::mode::InputAction::SpotifyTrackChanged);
                    }
                }
            }
        }

        "will_play" => {
            mark_status_sync_boost(state, Instant::now());
            let mut st = state.lock().unwrap();
            if st.mode != crate::mode::AppMode::Local {
                st.set_paused(false);
                st.last_pos_time = Instant::now();
                st.begin_spotify_command_pending(Instant::now(), SPOTIFY_EVENT_PENDING_WINDOW);
                st.spotify_was_active = true;
                st.set_stop_to_sleep_eligible(false);
            }
            drop(st);
            let _ = cmd_tx.send(crate::mode::InputAction::SpotifyActivated);
        }

        "playing" => {
            mark_status_sync_boost(state, Instant::now());
            let mut st = state.lock().unwrap();
            if st.mode != crate::mode::AppMode::Local {
                st.set_paused(false);
                st.last_pos_time = Instant::now();
                st.clear_spotify_command_pending();
                st.spotify_was_active = true;
                st.set_stop_to_sleep_eligible(false);
            }
            drop(st);
            let _ = cmd_tx.send(crate::mode::InputAction::SpotifyActivated);
        }

        "not_playing" | "will_pause" => {
            let mut st = state.lock().unwrap();
            if st.mode != crate::mode::AppMode::Local {
                st.set_paused(true);
                st.begin_spotify_command_pending(Instant::now(), SPOTIFY_EVENT_PENDING_WINDOW);
                st.set_stop_to_sleep_eligible(false);
            }
        }

        "paused" => {
            let mut st = state.lock().unwrap();
            if st.mode != crate::mode::AppMode::Local {
                st.set_paused(true);
                st.clear_spotify_command_pending();
                st.set_stop_to_sleep_eligible(false);
            }
        }

        "stopped" => {
            let is_local = state.lock().unwrap().mode == crate::mode::AppMode::Local;
            if !is_local {
                let mut st = state.lock().unwrap();
                clear_spotify_playback_snapshot(&mut st, Instant::now());
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
                st.clear_spotify_command_pending();
                st.spotify_was_active = true;
                st.set_stop_to_sleep_eligible(false);
            }
            fetch_status(state, render_state, None, Some(cmd_tx));
            let _ = cmd_tx.send(crate::mode::InputAction::SpotifyActivated);
        }

        "inactive" => {
            let is_local = state.lock().unwrap().mode == crate::mode::AppMode::Local;
            if !is_local {
                let mut st = state.lock().unwrap();
                clear_spotify_playback_snapshot(&mut st, Instant::now());
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
    use std::sync::mpsc::{self, Receiver};
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
            img_spotify_on: None,
            img_spotify_off: None,
            img_fav_on: None,
            img_fav_off: None,
            img_bat0: None,
            img_bat25: None,
            img_bat50: None,
            img_bat75: None,
            img_bat100: None,
            img_bat_charging: None,
            requested_cover_url: None,
            applied_cover_url: None,
        }))
    }

    fn test_cmd_tx() -> mpsc::Sender<crate::mode::InputAction> {
        let (tx, _rx) = mpsc::channel();
        tx
    }

    fn test_cmd_channel() -> (
        mpsc::Sender<crate::mode::InputAction>,
        Receiver<crate::mode::InputAction>,
    ) {
        mpsc::channel()
    }

    fn make_event(event_type: &str, data: Option<&str>) -> WSEvent {
        WSEvent {
            event_type: event_type.to_string(),
            data: data
                .map(|json| serde_json::value::RawValue::from_string(json.to_string()).unwrap()),
        }
    }

    #[test]
    fn repeated_paused_event_does_not_mark_dirty() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let render_state = empty_render_state();
        let cmd_tx = test_cmd_tx();
        {
            let mut st = state.lock().unwrap();
            st.paused = true;
            st.connected = true;
            st.render_dirty = false;
        }

        handle_event(make_event("paused", None), &state, &render_state, &cmd_tx);

        let st = state.lock().unwrap();
        assert!(st.paused);
        assert!(!st.render_dirty);
    }

    #[test]
    fn will_pause_event_switches_to_paused_state() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let render_state = empty_render_state();
        let cmd_tx = test_cmd_tx();
        {
            let mut st = state.lock().unwrap();
            st.paused = false;
            st.render_dirty = false;
        }

        handle_event(
            make_event("will_pause", None),
            &state,
            &render_state,
            &cmd_tx,
        );

        let st = state.lock().unwrap();
        assert!(st.paused);
        assert!(st.render_dirty);
    }

    #[test]
    fn unchanged_volume_event_does_not_mark_dirty() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let render_state = empty_render_state();
        let cmd_tx = test_cmd_tx();
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
            &cmd_tx,
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
        let cmd_tx = test_cmd_tx();
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
            &cmd_tx,
        );

        let st = state.lock().unwrap();
        assert_eq!(st.volume, 75);
        assert!(st.render_dirty);
    }

    #[test]
    fn spotify_playing_event_dispatches_takeover() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let render_state = empty_render_state();
        let (cmd_tx, cmd_rx) = test_cmd_channel();
        {
            let mut st = state.lock().unwrap();
            st.set_mode(crate::mode::AppMode::Local);
        }

        handle_event(make_event("playing", None), &state, &render_state, &cmd_tx);

        assert_eq!(
            cmd_rx.try_recv().ok(),
            Some(crate::mode::InputAction::SpotifyActivated)
        );
    }

    #[test]
    fn stopped_event_clears_stale_spotify_state() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let render_state = empty_render_state();
        let (cmd_tx, cmd_rx) = test_cmd_channel();
        {
            let mut st = state.lock().unwrap();
            st.set_mode(crate::mode::AppMode::Spotify);
            st.current_track_uri = "spotify:track:active".to_string();
            st.track_name = "Active".to_string();
            st.artist_name = "Artist".to_string();
            st.connected = true;
            st.paused = false;
            st.spotify_was_active = true;
        }

        handle_event(make_event("stopped", None), &state, &render_state, &cmd_tx);

        let st = state.lock().unwrap();
        assert!(st.current_track_uri.is_empty());
        assert!(st.track_name.is_empty());
        assert!(st.paused);
        assert_eq!(
            cmd_rx.try_recv().ok(),
            Some(crate::mode::InputAction::SpotifyDeactivated)
        );
    }

    #[test]
    fn stopped_status_snapshot_clears_stale_track_state() {
        let mut state = AppState::new();
        state.current_track_uri = "spotify:track:stale".to_string();
        state.track_name = "Stale".to_string();
        state.artist_name = "Artist".to_string();
        state.album_name = "Album".to_string();
        state.connected = true;
        state.paused = false;
        state.duration = 100_000;
        state.position = 30_000;
        state.is_favorited = true;
        state.render_dirty = false;

        clear_spotify_playback_snapshot(&mut state, Instant::now());

        assert!(!state.connected);
        assert!(state.paused);
        assert_eq!(state.current_track_uri, "");
        assert_eq!(state.track_name, "");
        assert_eq!(state.artist_name, "");
        assert_eq!(state.album_name, "");
        assert_eq!(state.duration, 0);
        assert_eq!(state.position, 0);
        assert!(!state.is_favorited);
        assert!(state.render_dirty);
    }

    #[test]
    fn backend_unavailable_marks_spotify_offline_with_notice() {
        let state = Arc::new(Mutex::new(AppState::new()));
        {
            let mut st = state.lock().unwrap();
            st.set_mode(crate::mode::AppMode::Spotify);
            st.set_connected(true);
        }

        mark_backend_unavailable(&state);

        let mut st = state.lock().unwrap();
        assert!(!st.connected);
        assert_eq!(
            st.active_notice_message(Instant::now()),
            Some("Spotify offline".to_string())
        );
    }

    #[test]
    fn backend_unavailable_does_not_interrupt_local_mode() {
        let state = Arc::new(Mutex::new(AppState::new()));
        {
            let mut st = state.lock().unwrap();
            st.set_mode(crate::mode::AppMode::Local);
            st.set_connected(true);
        }

        mark_backend_unavailable(&state);

        let mut st = state.lock().unwrap();
        assert!(st.connected);
        assert_eq!(st.active_notice_message(Instant::now()), None);
    }

    #[test]
    fn api_timeout_is_busy_not_offline() {
        assert_eq!(
            classify_api_command_error(&ureq::Error::Timeout(ureq::Timeout::Global)),
            ApiCommandResult::Busy
        );
    }

    #[test]
    fn api_connection_failure_is_offline() {
        assert_eq!(
            classify_api_command_error(&ureq::Error::ConnectionFailed),
            ApiCommandResult::Offline
        );
    }

    #[test]
    fn empty_status_body_clears_snapshot_without_offline_notice() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let render_state = empty_render_state();
        let (cmd_tx, cmd_rx) = test_cmd_channel();
        {
            let mut st = state.lock().unwrap();
            st.set_mode(crate::mode::AppMode::Spotify);
            st.set_connected(true);
            st.current_track_uri = "spotify:track:stale".to_string();
            st.track_name = "Stale".to_string();
            st.artist_name = "Artist".to_string();
            st.paused = false;
        }

        handle_status_body("", &state, &render_state, None, Some(&cmd_tx));

        let mut st = state.lock().unwrap();
        assert!(!st.connected);
        assert!(st.current_track_uri.is_empty());
        assert_eq!(st.active_notice_message(Instant::now()), None);
        assert_eq!(
            cmd_rx.try_recv().ok(),
            Some(crate::mode::InputAction::SpotifyDeactivated)
        );
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
    fn cover_url_policy_allows_only_trusted_https_cover_hosts() {
        assert!(is_allowed_cover_url("https://i.scdn.co/image/example"));
        assert!(is_allowed_cover_url(
            "https://is1-ssl.mzstatic.com/image/thumb/Music116/v4/example/600x600bb.jpg"
        ));
        assert!(is_allowed_cover_url(
            "https://a1.mzstatic.com/us/r1000/0/Music/example/600x600bb.jpg"
        ));

        assert!(!is_allowed_cover_url("http://i.scdn.co/image/example"));
        assert!(!is_allowed_cover_url("file:///tmp/cover.jpg"));
        assert!(!is_allowed_cover_url("https://127.0.0.1/cover.jpg"));
        assert!(!is_allowed_cover_url("https://example.com/cover.jpg"));
    }

    #[test]
    fn cover_fetch_rejects_untrusted_urls_before_fetcher_runs() {
        let cache_dir = unique_cache_dir();
        let calls = Cell::new(0);

        let result = fetch_cover_bytes_with("https://127.0.0.1/cover.jpg", &cache_dir, |_| {
            calls.set(calls.get() + 1);
            Some(vec![1, 2, 3])
        });

        assert_eq!(result, None);
        assert_eq!(calls.get(), 0);

        let _ = fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn api_agent_uses_short_global_timeout() {
        assert_eq!(
            api_agent().config().timeouts().global,
            Some(API_REQUEST_TIMEOUT)
        );
    }

    #[test]
    fn api_command_agent_waits_for_slow_spotify_track_loads() {
        assert!(
            API_COMMAND_TIMEOUT >= Duration::from_secs(20),
            "Spotify commands must wait longer than uncached go-librespot track loads"
        );
        assert_eq!(
            api_command_agent().config().timeouts().global,
            Some(API_COMMAND_TIMEOUT)
        );
    }

    #[test]
    fn api_post_url_reports_failure() {
        assert!(!api_post_url("http://127.0.0.1:0/player/pause"));
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

        assert!(apply_cached_cover_if_present_from(
            url,
            &cache_dir,
            &render_state
        ));

        let rs = render_state.lock().unwrap();
        assert_eq!(rs.applied_cover_url.as_deref(), Some(url));
        assert!(rs.scene_cover.is_some());

        let _ = fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn cover_log_key_uses_stable_short_hash_prefix() {
        assert_eq!(
            cover_log_key("https://i.scdn.co/image/cached-cover").len(),
            8
        );
        assert_eq!(
            cover_log_key("https://i.scdn.co/image/cached-cover"),
            cover_log_key("https://i.scdn.co/image/cached-cover")
        );
    }

    #[test]
    fn spotify_cover_urls_are_upgraded_to_640_square() {
        assert_eq!(
            prefer_high_res_cover_url(
                "https://i.scdn.co/image/ab67616d00001e0254b26107b2b819ad77e17311"
            ),
            "https://i.scdn.co/image/ab67616d0000b27354b26107b2b819ad77e17311"
        );
        assert_eq!(
            prefer_high_res_cover_url(
                "https://i.scdn.co/image/ab67616d0000485154b26107b2b819ad77e17311"
            ),
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
    fn status_sync_busy_timeout_enters_backoff_window() {
        let now = Instant::now();
        assert_eq!(
            status_sync_backoff_until(ApiCommandResult::Busy, now),
            Some(now + STATUS_SYNC_BUSY_BACKOFF)
        );
        assert_eq!(status_sync_backoff_until(ApiCommandResult::Ok, now), None);
        assert_eq!(
            status_sync_backoff_until(ApiCommandResult::Offline, now),
            None
        );
    }

    #[test]
    fn status_sync_waits_while_spotify_command_is_pending() {
        let now = Instant::now();
        let mut state = AppState::new();
        state.connected = true;
        state.paused = false;
        state.duration = 120_000;
        state.spotify_command_pending_until = Some(now + Duration::from_secs(4));

        assert_eq!(
            spotify_command_pending_sleep_for(&state, now),
            Some(Duration::from_secs(4))
        );
    }

    #[test]
    fn will_play_event_defers_status_polling_during_track_load() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let render_state = empty_render_state();
        let (cmd_tx, _cmd_rx) = test_cmd_channel();
        state
            .lock()
            .unwrap()
            .set_mode(crate::mode::AppMode::Spotify);

        handle_event(
            WSEvent {
                event_type: "will_play".to_string(),
                data: None,
            },
            &state,
            &render_state,
            &cmd_tx,
        );

        let st = state.lock().unwrap();
        assert!(spotify_command_pending_sleep_for(&st, Instant::now()).is_some());
        assert!(st.spotify_skip_pending_until.is_none());
    }

    #[test]
    fn not_playing_event_defers_status_polling_during_track_transition() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let render_state = empty_render_state();
        let (cmd_tx, _cmd_rx) = test_cmd_channel();
        state
            .lock()
            .unwrap()
            .set_mode(crate::mode::AppMode::Spotify);

        handle_event(
            WSEvent {
                event_type: "not_playing".to_string(),
                data: None,
            },
            &state,
            &render_state,
            &cmd_tx,
        );

        let st = state.lock().unwrap();
        assert!(st.paused);
        assert!(spotify_command_pending_sleep_for(&st, Instant::now()).is_some());
    }

    #[test]
    fn playing_event_clears_spotify_command_pending() {
        let state = Arc::new(Mutex::new(AppState::new()));
        let render_state = empty_render_state();
        let (cmd_tx, _cmd_rx) = test_cmd_channel();
        let now = Instant::now();
        {
            let mut st = state.lock().unwrap();
            st.set_mode(crate::mode::AppMode::Spotify);
            st.begin_spotify_skip_pending(now, Duration::from_secs(15));
        }

        handle_event(
            WSEvent {
                event_type: "playing".to_string(),
                data: None,
            },
            &state,
            &render_state,
            &cmd_tx,
        );

        let st = state.lock().unwrap();
        assert!(st.spotify_command_pending_until.is_none());
        assert!(st.spotify_skip_pending_until.is_none());
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
