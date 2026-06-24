#![allow(dead_code)]

mod animation;
mod app;
mod battery;
mod constants;
mod display;
mod download;
mod drawing;
mod favorites;
mod font;
mod framebuffer;
mod image_ops;
mod input;
mod local_import;
mod local_player;
mod log_utils;
mod mode;
mod network;
mod paths;
mod playlist_view;
mod power;
mod render;
mod resources;
mod types;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use app::{AppState, Assets};
use constants::*;
use download::{download_request_for_incomplete_favorite, DownloadManager, DownloadRequest};
use favorites::{FavoriteEntry, FavoriteSource, FavoritesManager};
use font::FontSet;
use framebuffer::Framebuffer;
use local_player::{local_volume_percent, LocalPlaybackError, LocalPlayer, SpotifyPipeAudio};
use mode::{AppMode, InputAction};
use paths::{app_paths, detect_paths, init_paths};
use render::RenderState;

const SIDEB_AUTOSTART_LOCAL_PLAYBACK_ENV: &str = "SIDEB_AUTOSTART_LOCAL_PLAYBACK";
const PLAYBACK_STATE_FILE: &str = "playback_state.json";
const STARTUP_COMMAND_HARD_SUPPRESSION: Duration = Duration::from_secs(10);
const STARTUP_COMMAND_SOFT_SUPPRESSION: Duration = Duration::from_secs(18);
const STARTUP_COMMAND_BURST_TAIL: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct PlaybackState {
    #[serde(default)]
    last_local_track_uri: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaylistMove {
    Up,
    Down,
}

#[derive(Debug)]
struct StartupCommandGuard {
    hard_suppress_until: Instant,
    soft_suppress_until: Instant,
    last_suppressed_at: Option<Instant>,
    soft_launch_action_suppressed: bool,
}

impl StartupCommandGuard {
    fn new(started_at: Instant) -> Self {
        Self {
            hard_suppress_until: started_at + STARTUP_COMMAND_HARD_SUPPRESSION,
            soft_suppress_until: started_at + STARTUP_COMMAND_SOFT_SUPPRESSION,
            last_suppressed_at: None,
            soft_launch_action_suppressed: false,
        }
    }

    fn should_suppress(&mut self, action: InputAction, now: Instant) -> bool {
        if !is_startup_suppressible_input_action(action) {
            return false;
        }

        if now < self.hard_suppress_until {
            return self.record_suppressed(now);
        }

        if self
            .last_suppressed_at
            .map(|last| now.saturating_duration_since(last) < STARTUP_COMMAND_BURST_TAIL)
            .unwrap_or(false)
        {
            return self.record_suppressed(now);
        }

        if now < self.soft_suppress_until
            && !self.soft_launch_action_suppressed
            && is_startup_launch_edge_action(action)
        {
            self.soft_launch_action_suppressed = true;
            return self.record_suppressed(now);
        }

        false
    }

    fn record_suppressed(&mut self, now: Instant) -> bool {
        self.last_suppressed_at = Some(now);
        true
    }
}

fn main() {
    eprintln!("sideb starting");
    init_paths(detect_paths());
    eprintln!(
        "paths: app={} data={} resources={}",
        app_paths().app_dir.display(),
        app_paths().data_dir.display(),
        app_paths().resources_dir.display()
    );

    // Initialize framebuffer
    let fb = Framebuffer::open().unwrap_or_else(|e| {
        eprintln!("framebuffer init: {e}");
        std::process::exit(1);
    });

    // Load fonts
    let font_data = resources::load_font_data().unwrap_or_else(|| {
        eprintln!("no font found");
        std::process::exit(1);
    });
    let fonts = FontSet::load(font_data).unwrap_or_else(|e| {
        eprintln!("font init: {e}");
        std::process::exit(1);
    });

    // Load assets
    let assets = Assets::load();

    // Show a lightweight startup page before heavy render cache initialization.
    let mut back_buf = vec![0u8; FB_SIZE];
    let startup_scene = render::build_startup_scene(
        &assets.tape_base,
        &assets.tape_a,
        &assets.taperoll,
        &assets.wheel,
        &fonts,
    );
    back_buf.copy_from_slice(&startup_scene);
    fb.swap_buffers(&back_buf);

    // Initialize render state (pre-computes all caches)
    eprintln!("building render caches...");
    let render_state = RenderState::init(
        &assets.tape_base,
        &assets.tape_a,
        &assets.taperoll,
        &assets.wheel,
        assets.cover_mask,
        assets.playing,
        assets.paused,
        assets.spotify_on,
        assets.spotify_off,
        assets.fav_on,
        assets.fav_off,
        assets.bat0,
        assets.bat25,
        assets.bat50,
        assets.bat75,
        assets.bat100,
        assets.bat_charging,
        &fonts,
    );
    eprintln!("render caches ready");

    // Ensure data directories exist
    let _ = std::fs::create_dir_all(&app_paths().music_dir);
    let _ = std::fs::create_dir_all(&app_paths().imports_dir);

    let app_state = Arc::new(Mutex::new(AppState::new()));
    let render_state = Arc::new(Mutex::new(render_state));
    let quit = Arc::new(AtomicBool::new(false));
    let pending_removals = Arc::new(Mutex::new(HashMap::<String, FavoriteEntry>::new()));

    // Initialize favorites and local player
    let favorites = Arc::new(Mutex::new(FavoritesManager::load(
        &app_paths().favorites_path,
    )));
    let local_player = Arc::new(Mutex::new(LocalPlayer::new()));
    let spotify_audio = Arc::new(SpotifyPipeAudio::start(
        Arc::clone(&quit),
        current_local_volume_percent(&app_state),
    ));

    let cover_updates = local_import::sync_existing_music_covers(&favorites);
    if cover_updates > 0 {
        eprintln!("import: startup linked {cover_updates} existing cover file(s)");
    }
    let startup_import_count = local_import::pending_import_count();
    if startup_import_count > 0 {
        app_state
            .lock()
            .unwrap()
            .set_import_progress(0, startup_import_count);
    }

    // Clean up orphaned files in music directory
    cleanup_orphaned_files(&favorites);

    // Create command channel
    let (cmd_tx, cmd_rx) = mpsc::channel::<InputAction>();

    // Create download manager (spawns its own background thread)
    let download_manager = DownloadManager::new(Arc::clone(&favorites), Arc::clone(&app_state));
    let download_progress = Arc::clone(download_manager.progress());

    battery::refresh_app_state(&app_state);
    let startup_playback_state = load_playback_state();

    // Set initial mode: Local (paused) if favorites exist, else Waiting
    {
        let fav = favorites.lock().unwrap();
        let downloaded = fav.downloaded_entries();
        if let Some(entry) = select_local_restore_target(
            &downloaded,
            startup_playback_state.last_local_track_uri.as_deref(),
        )
        .cloned()
        {
            let mut st = app_state.lock().unwrap();
            st.set_mode(AppMode::Local);
            st.set_paused(true);
            st.track_name = entry.name.clone();
            st.artist_name = entry.artist.clone();
            st.album_name = entry.album.clone();
            st.current_track_uri = entry.uri.clone();
            st.duration = entry.duration_ms.unwrap_or(0);
            st.position = 0;
            st.set_favorited(true);
            drop(st);
            // Load cover art for first track
            load_local_cover(&entry, &render_state);
        }
    }

    // Initial render
    {
        let st = app_state.lock().unwrap();
        let rs = render_state.lock().unwrap();
        if let Some(msg) = st.import_progress_message() {
            back_buf.copy_from_slice(&rs.scene_waiting);
            render::draw_waiting_import_progress(&mut back_buf, &fonts, &msg);
        } else if st.mode == AppMode::Waiting {
            back_buf.copy_from_slice(&rs.scene_waiting);
        } else {
            back_buf.copy_from_slice(&rs.scene_playing);
        }
        drop(rs);
        drop(st);
        fb.swap_buffers(&back_buf);
    }

    // Spawn input thread
    let input_state = Arc::clone(&app_state);
    let input_quit = Arc::clone(&quit);
    let input_cmd_tx = cmd_tx.clone();
    let _input_handle = std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            input::run(input_state, input_quit, input_cmd_tx);
        })
        .expect("spawn input thread");

    // Spawn WebSocket thread
    let ws_state = Arc::clone(&app_state);
    let ws_render = Arc::clone(&render_state);
    let ws_quit = Arc::clone(&quit);
    let ws_cmd_tx = cmd_tx.clone();
    let _ws_handle = std::thread::Builder::new()
        .name("websocket".into())
        .spawn(move || {
            network::listen_events(ws_state, ws_render, ws_quit, ws_cmd_tx);
        })
        .expect("spawn websocket thread");

    // Spawn lightweight status polling thread for drift correction.
    let poll_state = Arc::clone(&app_state);
    let poll_render = Arc::clone(&render_state);
    let poll_quit = Arc::clone(&quit);
    let poll_cmd_tx = cmd_tx.clone();
    let _poll_handle = std::thread::Builder::new()
        .name("status-poll".into())
        .spawn(move || {
            network::poll_status(poll_state, poll_render, poll_quit, poll_cmd_tx);
        })
        .expect("spawn status poll thread");

    // Spawn low-frequency battery polling thread.
    let battery_state = Arc::clone(&app_state);
    let battery_quit = Arc::clone(&quit);
    let _battery_handle = std::thread::Builder::new()
        .name("battery".into())
        .spawn(move || {
            battery::run(battery_state, battery_quit);
        })
        .expect("spawn battery thread");

    // Spawn command processor thread
    let cmd_app_state = Arc::clone(&app_state);
    let cmd_render_state = Arc::clone(&render_state);
    let cmd_favorites = Arc::clone(&favorites);
    let cmd_local_player = Arc::clone(&local_player);
    let cmd_spotify_audio = Arc::clone(&spotify_audio);
    let cmd_pending_removals = Arc::clone(&pending_removals);
    let cmd_quit = Arc::clone(&quit);
    let _cmd_handle = std::thread::Builder::new()
        .name("command".into())
        .spawn(move || {
            command_processor(
                cmd_rx,
                cmd_app_state,
                cmd_render_state,
                cmd_favorites,
                cmd_local_player,
                cmd_spotify_audio,
                cmd_pending_removals,
                download_manager,
                cmd_quit,
            );
        })
        .expect("spawn command processor thread");

    // Keep the Spotify pipe output volume in sync with go-librespot volume events.
    let volume_state = Arc::clone(&app_state);
    let volume_spotify_audio = Arc::clone(&spotify_audio);
    let volume_quit = Arc::clone(&quit);
    let _volume_handle = std::thread::Builder::new()
        .name("volume-sync".into())
        .spawn(move || {
            while !volume_quit.load(Ordering::Relaxed) {
                volume_spotify_audio
                    .set_volume_percent(current_local_volume_percent(&volume_state));
                std::thread::sleep(Duration::from_millis(250));
            }
        })
        .expect("spawn volume sync thread");

    // Spawn local import monitor thread
    let import_favorites = Arc::clone(&favorites);
    let import_quit = Arc::clone(&quit);
    let import_cmd_tx = cmd_tx.clone();
    let _import_handle = std::thread::Builder::new()
        .name("local-import".into())
        .spawn(move || {
            local_import::run(import_favorites, import_cmd_tx, import_quit);
        })
        .expect("spawn local import thread");

    // Spawn local playback monitor thread
    let mon_app_state = Arc::clone(&app_state);
    let mon_render_state = Arc::clone(&render_state);
    let mon_local_player = Arc::clone(&local_player);
    let mon_favorites = Arc::clone(&favorites);
    let mon_pending_removals = Arc::clone(&pending_removals);
    let mon_quit = Arc::clone(&quit);
    let _mon_handle = std::thread::Builder::new()
        .name("local-monitor".into())
        .spawn(move || {
            local_playback_monitor(
                mon_app_state,
                mon_render_state,
                mon_local_player,
                mon_favorites,
                mon_pending_removals,
                mon_quit,
            );
        })
        .expect("spawn local monitor thread");

    // Spawn stop-to-suspend monitor. It only releases keepalive files after
    // playback has been stopped through the grace period and final re-check.
    let power_state = Arc::clone(&app_state);
    let power_local_player = Arc::clone(&local_player);
    let power_download_progress = Arc::clone(&download_progress);
    let power_quit = Arc::clone(&quit);
    let _power_handle = std::thread::Builder::new()
        .name("power".into())
        .spawn(move || {
            power::run_sleep_monitor(
                power_state,
                power_local_player,
                power_download_progress,
                power_quit,
            );
        })
        .expect("spawn power monitor thread");

    if autostart_local_playback_enabled(
        std::env::var(SIDEB_AUTOSTART_LOCAL_PLAYBACK_ENV)
            .ok()
            .as_deref(),
    ) {
        eprintln!("local_player: autostart local playback requested");
        let _ = cmd_tx.send(InputAction::StartLocalPlayback);
    }

    // Run render loop on main thread
    let render_quit = Arc::clone(&quit);

    // Set up signal handler
    let sig_quit = Arc::clone(&quit);
    let _ = std::thread::Builder::new()
        .name("signal".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if sig_quit.load(Ordering::Relaxed) {
                return;
            }
        });

    // Install signal handlers via libc
    unsafe {
        let quit_for_signal = Arc::clone(&quit);
        QUIT_FLAG.store(
            quit_for_signal.as_ref() as *const AtomicBool as usize,
            Ordering::SeqCst,
        );

        libc::signal(
            libc::SIGINT,
            signal_handler as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            signal_handler as *const () as libc::sighandler_t,
        );
    }

    // Run render loop (blocks until quit)
    render::render_loop(
        &fb,
        &mut back_buf,
        Arc::clone(&app_state),
        Arc::clone(&render_state),
        &fonts,
        render_quit,
        Arc::clone(&favorites),
        Arc::clone(&download_progress),
    );

    // Stop all playback on exit
    remember_current_local_track_for_exit(&app_state, &local_player);
    network::api_post("/player/pause");
    {
        let mut player = local_player.lock().unwrap();
        player.stop();
    }

    // Clear screen on exit
    for byte in back_buf.iter_mut() {
        *byte = 0;
    }
    fb.swap_buffers(&back_buf);

    eprintln!("exiting");
    std::process::exit(0);
}

/// Central command processor — receives InputActions and dispatches to subsystems.
#[allow(clippy::too_many_arguments)]
fn command_processor(
    rx: mpsc::Receiver<InputAction>,
    app_state: Arc<Mutex<AppState>>,
    render_state: Arc<Mutex<RenderState>>,
    favorites: Arc<Mutex<FavoritesManager>>,
    local_player: Arc<Mutex<LocalPlayer>>,
    spotify_audio: Arc<SpotifyPipeAudio>,
    pending_removals: Arc<Mutex<HashMap<String, FavoriteEntry>>>,
    download_manager: DownloadManager,
    quit: Arc<AtomicBool>,
) {
    let mut screen_backlight = display::ScreenBacklight::new();
    let mut startup_command_guard = StartupCommandGuard::new(Instant::now());

    for action in rx.iter() {
        if quit.load(Ordering::Relaxed) {
            return;
        }
        if startup_command_guard.should_suppress(action, Instant::now()) {
            eprintln!("input: suppressed startup action {action:?}");
            continue;
        }

        match action {
            InputAction::LockScreen => {
                let (mode, paused) = {
                    let st = app_state.lock().unwrap();
                    (st.mode, st.paused)
                };

                match mode {
                    AppMode::Spotify if !paused => {
                        spotify_audio.suspend();
                        network::api_post("/player/pause");
                    }
                    AppMode::Local => {
                        let mut player = local_player.lock().unwrap();
                        if player.is_playing() {
                            player.pause();
                        }
                    }
                    _ => {}
                }

                {
                    let mut st = app_state.lock().unwrap();
                    st.clear_confirmation();
                    st.set_playlist_visible(false);
                    if mode != AppMode::Waiting {
                        st.set_paused(true);
                    }
                    st.set_stop_to_sleep_eligible(false);
                    st.set_screen_locked(true);
                }
                screen_backlight.lock();
                eprintln!("cmd: screen locked");
            }

            InputAction::UnlockScreen => {
                screen_backlight.unlock();
                app_state.lock().unwrap().set_screen_locked(false);
                eprintln!("cmd: screen unlocked");
            }

            InputAction::RequestExit => {
                let mut st = app_state.lock().unwrap();
                let now = Instant::now();
                if st.request_exit_confirmation(now) {
                    eprintln!("exit confirmed via B (double press)");
                    drop(st);
                    quit.store(true, Ordering::Relaxed);
                    return;
                }
                eprintln!("exit: press B again within 2s to confirm");
            }

            InputAction::ExitApp => {
                quit.store(true, Ordering::Relaxed);
                return;
            }

            InputAction::ToggleFavorite => {
                let (uri, name, artist, album, cover_url, spotify_duration_ms) = {
                    let mode = app_state.lock().unwrap().mode;
                    match mode {
                        AppMode::Spotify => {
                            let (uri, name, artist, album, dur) = {
                                let st = app_state.lock().unwrap();
                                let dur = if st.duration > 0 {
                                    Some(st.duration)
                                } else {
                                    None
                                };
                                (
                                    st.current_track_uri.clone(),
                                    st.track_name.clone(),
                                    st.artist_name.clone(),
                                    st.album_name.clone(),
                                    dur,
                                )
                            };
                            let cover = render_state
                                .lock()
                                .unwrap()
                                .requested_cover_url
                                .clone()
                                .unwrap_or_default();
                            (uri, name, artist, album, cover, dur)
                        }
                        AppMode::Local => {
                            let player = local_player.lock().unwrap();
                            if let Some(entry) = player.current_entry() {
                                (
                                    entry.uri.clone(),
                                    entry.name.clone(),
                                    entry.artist.clone(),
                                    entry.album.clone(),
                                    entry.cover_url.clone(),
                                    entry.spotify_duration_ms,
                                )
                            } else {
                                continue;
                            }
                        }
                        _ => continue,
                    }
                };

                if uri.is_empty() {
                    continue;
                }

                let current_local_uri = current_local_track_uri(&local_player);
                let mut fav = favorites.lock().unwrap();
                if fav.is_favorited(&uri) {
                    let now = Instant::now();
                    let confirmed = app_state
                        .lock()
                        .unwrap()
                        .request_remove_confirmation(&uri, now);
                    if !confirmed {
                        eprintln!("remove: press X again within 2s to confirm {uri}");
                        continue;
                    }
                    if should_defer_favorite_file_deletion(current_local_uri.as_deref(), &uri) {
                        if let Some(entry) = fav.remove_preserving_files(&uri) {
                            pending_removals.lock().unwrap().insert(uri.clone(), entry);
                        }
                    } else {
                        fav.remove(&uri);
                    }
                    let mut st = app_state.lock().unwrap();
                    st.clear_confirmation();
                    st.set_favorited(false);
                    drop(st);
                    drop(fav);
                    refresh_library_state(&app_state, &render_state, &favorites, &local_player);
                    eprintln!("cmd: unfavorited {}", uri);
                } else {
                    // Favorite + trigger download
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let entry = FavoriteEntry {
                        uri: uri.clone(),
                        name: name.clone(),
                        artist: artist.clone(),
                        album: album.clone(),
                        cover_url: cover_url.clone(),
                        source: FavoriteSource::Spotify,
                        file_path: None,
                        cover_path: None,
                        duration_ms: None,
                        spotify_duration_ms,
                        downloaded: false,
                        added_at: format!("{}", now),
                    };
                    let restored = pending_removals.lock().unwrap().remove(&uri);
                    if let Some(restored) = restored {
                        fav.add(restored);
                    } else {
                        fav.add(entry);
                    }
                    app_state.lock().unwrap().set_favorited(true);
                    drop(fav);
                    refresh_library_state(&app_state, &render_state, &favorites, &local_player);

                    if !favorites
                        .lock()
                        .unwrap()
                        .find_by_uri(&uri)
                        .map(|entry| entry.downloaded)
                        .unwrap_or(false)
                    {
                        // Trigger download only for genuinely new favorites.
                        download_manager.enqueue(DownloadRequest {
                            uri,
                            track_name: name,
                            artist_name: artist,
                            cover_url,
                            spotify_duration_ms,
                        });
                    }
                }
            }

            InputAction::TogglePlayPause => {
                app_state.lock().unwrap().set_stop_to_sleep_eligible(false);
                let volume_percent = current_local_volume_percent(&app_state);
                let mut player = local_player.lock().unwrap();
                if player.is_active() {
                    player.toggle_pause();
                    let paused = player.is_paused();
                    app_state.lock().unwrap().set_paused(paused);
                } else {
                    // Player not started — start shuffled playback from displayed track
                    spotify_audio.suspend();
                    network::api_post("/player/pause");
                    let current_uri = app_state.lock().unwrap().current_track_uri.clone();
                    let downloaded = favorites.lock().unwrap().downloaded_entries();
                    if !downloaded.is_empty() {
                        player.set_volume_percent(volume_percent);
                        match player.start_shuffled_with_first(downloaded, &current_uri) {
                            Ok(()) => {
                                sync_local_track_to_app(&player, &app_state, &favorites);
                                let mut st = app_state.lock().unwrap();
                                st.set_mode(AppMode::Local);
                                st.set_paused(false);
                                drop(st);
                                if let Some(entry) = player.current_entry() {
                                    load_local_cover(entry, &render_state);
                                }
                            }
                            Err(error) => {
                                drop(player);
                                show_playback_notice(&app_state, error);
                            }
                        }
                    } else {
                        show_user_notice(&app_state, "No local tracks");
                    }
                }
            }

            InputAction::NextTrack => {
                app_state.lock().unwrap().set_stop_to_sleep_eligible(false);
                let downloaded = favorites.lock().unwrap().downloaded_entries();
                let volume_percent = current_local_volume_percent(&app_state);
                let mut player = local_player.lock().unwrap();
                player.set_volume_percent(volume_percent);
                player.refresh_playlist(downloaded);
                match player.next() {
                    Ok(()) => {
                        sync_local_track_to_app(&player, &app_state, &favorites);
                        let entry = player.current_entry().cloned();
                        drop(player);
                        if let Some(entry) = entry {
                            load_local_cover(&entry, &render_state);
                        }
                    }
                    Err(error) => {
                        drop(player);
                        show_playback_notice(&app_state, error);
                    }
                }
            }

            InputAction::PrevTrack => {
                app_state.lock().unwrap().set_stop_to_sleep_eligible(false);
                let downloaded = favorites.lock().unwrap().downloaded_entries();
                let volume_percent = current_local_volume_percent(&app_state);
                let mut player = local_player.lock().unwrap();
                player.set_volume_percent(volume_percent);
                player.refresh_playlist(downloaded);
                match player.prev() {
                    Ok(()) => {
                        sync_local_track_to_app(&player, &app_state, &favorites);
                        let entry = player.current_entry().cloned();
                        drop(player);
                        if let Some(entry) = entry {
                            load_local_cover(&entry, &render_state);
                        }
                    }
                    Err(error) => {
                        drop(player);
                        show_playback_notice(&app_state, error);
                    }
                }
            }

            InputAction::VolumeUp => {
                adjust_local_volume(&app_state, &local_player, &spotify_audio, 5);
            }

            InputAction::VolumeDown => {
                adjust_local_volume(&app_state, &local_player, &spotify_audio, -5);
            }

            InputAction::StartLocalPlayback => {
                app_state.lock().unwrap().set_stop_to_sleep_eligible(false);
                // Pause Spotify first to prevent audio overlap
                spotify_audio.suspend();
                network::api_post("/player/pause");

                let downloaded = favorites.lock().unwrap().downloaded_entries();
                if downloaded.is_empty() {
                    eprintln!("cmd: no downloaded tracks for local playback");
                    show_user_notice(&app_state, "No local tracks");
                    continue;
                }

                let volume_percent = current_local_volume_percent(&app_state);
                let mut player = local_player.lock().unwrap();
                player.set_volume_percent(volume_percent);
                let current_uri = app_state.lock().unwrap().current_track_uri.clone();
                let playback_result = if current_uri.trim().is_empty() {
                    player.start_shuffled(downloaded)
                } else {
                    player.start_shuffled_with_first(downloaded, &current_uri)
                };
                match playback_result {
                    Ok(()) => {
                        sync_local_track_to_app(&player, &app_state, &favorites);

                        let mut st = app_state.lock().unwrap();
                        st.set_mode(AppMode::Local);
                        st.set_paused(false);

                        // Load cover for first track
                        if let Some(entry) = player.current_entry() {
                            load_local_cover(entry, &render_state);
                        }
                    }
                    Err(error) => {
                        drop(player);
                        show_playback_notice(&app_state, error);
                    }
                }
            }

            InputAction::StopLocalPlayback => {
                let mut player = local_player.lock().unwrap();
                player.stop();
                drop(player);
                let downloads_active = !download_manager.progress().lock().unwrap().is_empty();
                let mut st = app_state.lock().unwrap();
                st.set_mode(AppMode::Waiting);
                st.set_paused(true);
                st.current_track_uri.clear();
                st.track_name.clear();
                st.artist_name.clear();
                st.album_name.clear();
                st.set_duration(0);
                st.set_position(0, Instant::now());
                st.set_favorited(false);
                st.set_stop_to_sleep_eligible(!downloads_active);
                drop(st);
                network::update_cover(None, &render_state);
            }

            InputAction::TogglePlaylist => {
                let mut st = app_state.lock().unwrap();
                let visible = !st.playlist_visible;
                st.set_playlist_visible(visible);
                if visible {
                    let count = favorites.lock().unwrap().count();
                    st.set_playlist_count(count);
                    if st.playlist_selected >= count && count > 0 {
                        st.set_playlist_selected(0);
                    }
                }
            }

            InputAction::PlaylistUp => {
                let mut st = app_state.lock().unwrap();
                let new_sel = advance_playlist_selection(
                    st.playlist_selected,
                    st.playlist_count,
                    PlaylistMove::Up,
                );
                st.set_playlist_selected(new_sel);
            }

            InputAction::PlaylistDown => {
                let mut st = app_state.lock().unwrap();
                let new_sel = advance_playlist_selection(
                    st.playlist_selected,
                    st.playlist_count,
                    PlaylistMove::Down,
                );
                st.set_playlist_selected(new_sel);
            }

            InputAction::PlaylistSelect => {
                let selected = app_state.lock().unwrap().playlist_selected;
                let fav = favorites.lock().unwrap();
                let entries = fav.all_entries();
                if selected < entries.len() {
                    let entry = entries[selected].clone();
                    drop(fav);

                    if entry.downloaded && entry.file_path.is_some() {
                        app_state.lock().unwrap().set_stop_to_sleep_eligible(false);
                        // Pause Spotify first to prevent audio overlap
                        spotify_audio.suspend();
                        network::api_post("/player/pause");

                        let volume_percent = current_local_volume_percent(&app_state);
                        let mut player = local_player.lock().unwrap();
                        player.set_volume_percent(volume_percent);
                        // Build a playlist from all downloaded entries
                        let downloaded = favorites.lock().unwrap().downloaded_entries();
                        let playback_result = if player.is_active() {
                            // Just switch to selected track
                            player.play_entry(&entry)
                        } else {
                            // Start fresh with all downloaded and place the selected track first.
                            player.start_shuffled_with_first(downloaded, &entry.uri)
                        };

                        match playback_result {
                            Ok(()) => {
                                sync_local_track_to_app(&player, &app_state, &favorites);

                                let mut st = app_state.lock().unwrap();
                                st.set_mode(AppMode::Local);
                                st.set_paused(false);
                                st.set_playlist_visible(false);

                                drop(st);
                                drop(player);

                                // Load cover
                                load_local_cover(&entry, &render_state);
                            }
                            Err(error) => {
                                drop(player);
                                let mut st = app_state.lock().unwrap();
                                st.set_playlist_visible(false);
                                drop(st);
                                show_playback_notice(&app_state, error);
                            }
                        }
                    } else {
                        if let Some(request) = download_request_for_incomplete_favorite(&entry) {
                            download_manager.retry_now(request);
                            show_user_notice(&app_state, "Retry queued");
                        } else {
                            show_playlist_unavailable_notice(&app_state, &entry);
                        }
                    }
                }
            }

            InputAction::PlaylistDelete => {
                let selected = {
                    let st = app_state.lock().unwrap();
                    st.playlist_selected
                };

                let uri = {
                    let fav = favorites.lock().unwrap();
                    let entries = fav.all_entries();
                    if selected < entries.len() {
                        Some(entries[selected].uri.clone())
                    } else {
                        None
                    }
                };

                if let Some(uri) = uri {
                    let current_local_uri = current_local_track_uri(&local_player);
                    let now = Instant::now();
                    let confirmed = app_state
                        .lock()
                        .unwrap()
                        .request_remove_confirmation(&uri, now);
                    if !confirmed {
                        eprintln!("remove: press X again within 2s to confirm {uri}");
                        continue;
                    }

                    let mut fav = favorites.lock().unwrap();
                    if should_defer_favorite_file_deletion(current_local_uri.as_deref(), &uri) {
                        if let Some(entry) = fav.remove_preserving_files(&uri) {
                            pending_removals.lock().unwrap().insert(uri.clone(), entry);
                        }
                    } else {
                        fav.remove(&uri);
                    }
                    let count = fav.count();
                    drop(fav);

                    let mut st = app_state.lock().unwrap();
                    st.clear_confirmation();
                    st.set_playlist_count(count);
                    if st.playlist_selected >= count && count > 0 {
                        st.set_playlist_selected(count - 1);
                    }

                    // Check if currently playing track was deleted
                    let current_uri = {
                        let player = local_player.lock().unwrap();
                        player.current_entry().map(|e| e.uri.clone())
                    };
                    if current_uri.as_deref() == Some(&uri) {
                        st.set_favorited(false);
                    }
                    drop(st);
                    refresh_library_state(&app_state, &render_state, &favorites, &local_player);
                }
            }

            InputAction::LibraryChanged => {
                refresh_library_state(&app_state, &render_state, &favorites, &local_player);
            }

            InputAction::ImportProgress { completed, total } => {
                app_state
                    .lock()
                    .unwrap()
                    .set_import_progress(completed, total);
            }

            InputAction::ImportFinished { failed } => {
                finish_import_progress(&app_state, failed);
            }

            InputAction::SpotifyActivated => {
                spotify_audio.resume();
                if app_state.lock().unwrap().screen_locked {
                    screen_backlight.unlock();
                    app_state.lock().unwrap().set_screen_locked(false);
                    eprintln!("cmd: screen unlocked by Spotify activation");
                }
                app_state.lock().unwrap().set_stop_to_sleep_eligible(false);
                let (local_active, remembered_uri) = {
                    let player = local_player.lock().unwrap();
                    (
                        player.is_active(),
                        player.current_entry().map(|entry| entry.uri.clone()),
                    )
                };

                let mut st = app_state.lock().unwrap();
                st.set_mode(AppMode::Spotify);
                st.set_paused(false);
                st.spotify_was_active = true;

                if local_active {
                    st.spotify_preempted_local_uri = remembered_uri.clone();
                    drop(st);
                    local_player.lock().unwrap().stop();
                    eprintln!(
                        "cmd: Spotify activated, stopped local playback remembered_uri={}",
                        remembered_uri.as_deref().unwrap_or("none")
                    );
                } else {
                    st.spotify_preempted_local_uri = None;
                }
            }

            InputAction::SpotifyTrackChanged => {
                if app_state.lock().unwrap().screen_locked {
                    screen_backlight.unlock();
                    app_state.lock().unwrap().set_screen_locked(false);
                    eprintln!("cmd: screen unlocked by Spotify track change");
                }
                app_state.lock().unwrap().set_stop_to_sleep_eligible(false);
                let st = app_state.lock().unwrap();
                let uri = st.current_track_uri.clone();
                drop(st);
                let is_fav = favorites.lock().unwrap().is_favorited(&uri);
                let mut st = app_state.lock().unwrap();
                st.spotify_was_active = true;
                st.set_favorited(is_fav);
            }

            InputAction::SpotifyDeactivated => {
                let (remembered_uri, spotify_was_active) = {
                    let st = app_state.lock().unwrap();
                    (
                        st.spotify_preempted_local_uri.clone(),
                        st.spotify_was_active,
                    )
                };
                let downloaded = favorites.lock().unwrap().downloaded_entries();

                if let Some(entry) =
                    select_local_restore_target(&downloaded, remembered_uri.as_deref()).cloned()
                {
                    let mut st = app_state.lock().unwrap();
                    st.set_mode(AppMode::Local);
                    st.set_paused(true);
                    st.spotify_preempted_local_uri = None;
                    st.current_track_uri = entry.uri.clone();
                    st.track_name = entry.name.clone();
                    st.artist_name = entry.artist.clone();
                    st.album_name = entry.album.clone();
                    st.set_duration(entry.duration_ms.unwrap_or(0));
                    st.set_position(0, Instant::now());
                    st.set_favorited(true);
                    st.spotify_was_active = false;
                    st.set_stop_to_sleep_eligible(false);
                    drop(st);
                    load_local_cover(&entry, &render_state);
                    eprintln!(
                        "cmd: Spotify deactivated, restored paused local track {}",
                        entry.uri
                    );
                } else {
                    let downloads_active = !download_manager.progress().lock().unwrap().is_empty();
                    let mut st = app_state.lock().unwrap();
                    st.set_mode(AppMode::Waiting);
                    st.spotify_preempted_local_uri = None;
                    st.current_track_uri.clear();
                    st.track_name.clear();
                    st.artist_name.clear();
                    st.album_name.clear();
                    st.set_duration(0);
                    st.set_position(0, Instant::now());
                    st.set_favorited(false);
                    st.spotify_was_active = false;
                    st.set_stop_to_sleep_eligible(
                        should_enable_stop_sleep_after_spotify_deactivated(
                            spotify_was_active,
                            downloads_active,
                        ),
                    );
                    drop(st);
                    network::update_cover(None, &render_state);
                    eprintln!("cmd: Spotify deactivated, no local restore target");
                }
            }
        }

        let current_local_uri = current_local_track_uri(&local_player);
        finalize_pending_removals(&pending_removals, &favorites, current_local_uri.as_deref());
    }
}

fn show_user_notice(app_state: &Arc<Mutex<AppState>>, message: impl Into<String>) {
    app_state
        .lock()
        .unwrap()
        .show_notice(message, Instant::now());
}

fn show_playback_notice(app_state: &Arc<Mutex<AppState>>, error: LocalPlaybackError) {
    show_user_notice(app_state, error.notice());
}

fn show_playlist_unavailable_notice(app_state: &Arc<Mutex<AppState>>, _entry: &FavoriteEntry) {
    show_user_notice(app_state, "Download first");
}

fn finish_import_progress(app_state: &Arc<Mutex<AppState>>, failed: usize) {
    let mut st = app_state.lock().unwrap();
    st.clear_import_progress();
    if failed > 0 {
        st.show_notice("Import failed", Instant::now());
    }
}

fn advance_playlist_selection(selected: usize, count: usize, movement: PlaylistMove) -> usize {
    if count == 0 {
        return 0;
    }

    match movement {
        PlaylistMove::Up => {
            if selected == 0 {
                count - 1
            } else {
                selected - 1
            }
        }
        PlaylistMove::Down => {
            if selected + 1 >= count {
                0
            } else {
                selected + 1
            }
        }
    }
}

fn should_enable_stop_sleep_after_spotify_deactivated(
    spotify_was_active: bool,
    downloads_active: bool,
) -> bool {
    spotify_was_active && !downloads_active
}

fn select_local_restore_target<'a>(
    downloaded: &'a [FavoriteEntry],
    remembered_uri: Option<&str>,
) -> Option<&'a FavoriteEntry> {
    remembered_uri
        .and_then(|uri| downloaded.iter().find(|entry| entry.uri == uri))
        .or_else(|| downloaded.first())
}

fn playback_state_path() -> PathBuf {
    app_paths().data_dir.join(PLAYBACK_STATE_FILE)
}

fn load_playback_state() -> PlaybackState {
    load_playback_state_from(&playback_state_path())
}

fn load_playback_state_from(path: &Path) -> PlaybackState {
    let data = match std::fs::read_to_string(path) {
        Ok(data) => data,
        Err(_) => return PlaybackState::default(),
    };

    match serde_json::from_str::<PlaybackState>(&data) {
        Ok(state) => state,
        Err(e) => {
            eprintln!(
                "playback_state: parse error path={} error={e}",
                path.display()
            );
            PlaybackState::default()
        }
    }
}

fn save_playback_state_to(path: &Path, state: &PlaybackState) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let tmp_path = path.with_extension("json.tmp");
    let json = match serde_json::to_string_pretty(state) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("playback_state: serialize error: {e}");
            return;
        }
    };

    if let Err(e) = std::fs::write(&tmp_path, json) {
        eprintln!(
            "playback_state: write tmp failed path={} error={e}",
            tmp_path.display()
        );
        return;
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        eprintln!(
            "playback_state: rename failed path={} error={e}",
            path.display()
        );
    }
}

fn remember_last_local_track_uri(uri: &str) {
    let uri = uri.trim();
    if uri.is_empty() {
        return;
    }
    save_playback_state_to(
        &playback_state_path(),
        &PlaybackState {
            last_local_track_uri: Some(uri.to_string()),
        },
    );
}

fn remember_current_local_track_for_exit(
    app_state: &Arc<Mutex<AppState>>,
    local_player: &Arc<Mutex<LocalPlayer>>,
) {
    let active_player_uri = current_local_track_uri(local_player);
    let displayed_or_preempted_uri = {
        let st = app_state.lock().unwrap();
        if st.mode == AppMode::Local && !st.current_track_uri.trim().is_empty() {
            Some(st.current_track_uri.clone())
        } else {
            st.spotify_preempted_local_uri.clone()
        }
    };

    if let Some(uri) = active_player_uri.or(displayed_or_preempted_uri) {
        remember_last_local_track_uri(&uri);
    }
}

/// Remove orphaned files from the music directory that are not referenced by any favorite.
fn cleanup_orphaned_files(favorites: &Arc<Mutex<FavoritesManager>>) {
    let music_dir = app_paths().music_dir.clone();
    cleanup_orphaned_files_in_dir(favorites, &music_dir);
}

fn cleanup_orphaned_files_in_dir(favorites: &Arc<Mutex<FavoritesManager>>, music_dir: &Path) {
    let referenced = {
        let fav = favorites.lock().unwrap();
        if !fav.allows_destructive_cleanup() {
            eprintln!("cleanup: skipped because favorites did not load cleanly from disk");
            return;
        }
        fav.referenced_managed_files(music_dir)
    };

    let entries = match std::fs::read_dir(music_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    let mut removed = 0u32;
    for entry in entries.flatten() {
        let path = entry.path();

        // Only clean up .mp3 and .jpg files
        match path.extension().and_then(|e| e.to_str()) {
            Some("mp3") | Some("jpg") | Some("jpeg") | Some("png") => {}
            _ => continue,
        }

        let comparable_path = match path.canonicalize() {
            Ok(path) => path,
            Err(_) => continue,
        };

        if !referenced.contains(&comparable_path) {
            if let Err(e) = std::fs::remove_file(&path) {
                eprintln!("cleanup: failed to remove {}: {e}", path.display());
            } else {
                removed += 1;
            }
        }
    }

    if removed > 0 {
        eprintln!(
            "cleanup: removed {removed} orphaned file(s) from {}",
            music_dir.display()
        );
    }
}

fn current_local_track_uri(local_player: &Arc<Mutex<LocalPlayer>>) -> Option<String> {
    local_player
        .lock()
        .unwrap()
        .current_entry()
        .map(|entry| entry.uri.clone())
}

fn should_defer_favorite_file_deletion(current_local_uri: Option<&str>, removed_uri: &str) -> bool {
    matches!(current_local_uri, Some(uri) if uri == removed_uri)
}

fn pending_removal_uris_ready_for_finalization(
    pending: &HashMap<String, FavoriteEntry>,
    current_local_uri: Option<&str>,
) -> Vec<String> {
    pending
        .keys()
        .filter(|uri| Some(uri.as_str()) != current_local_uri)
        .cloned()
        .collect()
}

fn finalize_pending_removals(
    pending_removals: &Arc<Mutex<HashMap<String, FavoriteEntry>>>,
    favorites: &Arc<Mutex<FavoritesManager>>,
    current_local_uri: Option<&str>,
) {
    let ready_entries = {
        let mut pending = pending_removals.lock().unwrap();
        let ready_uris = pending_removal_uris_ready_for_finalization(&pending, current_local_uri);
        let mut ready_entries = Vec::with_capacity(ready_uris.len());
        for uri in ready_uris {
            if let Some(entry) = pending.remove(&uri) {
                ready_entries.push(entry);
            }
        }
        ready_entries
    };

    if ready_entries.is_empty() {
        return;
    }

    let favorited_uris = {
        let fav = favorites.lock().unwrap();
        fav.all_entries()
            .iter()
            .map(|entry| entry.uri.clone())
            .collect::<std::collections::HashSet<_>>()
    };

    for entry in ready_entries {
        if favorited_uris.contains(&entry.uri) {
            continue;
        }
        FavoritesManager::delete_entry_files(&entry);
    }
}

fn refresh_library_state(
    app_state: &Arc<Mutex<AppState>>,
    render_state: &Arc<Mutex<RenderState>>,
    favorites: &Arc<Mutex<FavoritesManager>>,
    local_player: &Arc<Mutex<LocalPlayer>>,
) {
    let (count, downloaded) = {
        let fav = favorites.lock().unwrap();
        (fav.count(), fav.downloaded_entries())
    };

    {
        let mut player = local_player.lock().unwrap();
        player.refresh_playlist(downloaded.clone());
    }

    let mut seed_entry: Option<FavoriteEntry> = None;
    let mut clear_cover = false;
    let persisted_last_local_uri = load_playback_state().last_local_track_uri;
    {
        let player_active = local_player.lock().unwrap().is_active();
        let mut st = app_state.lock().unwrap();
        st.set_playlist_count(count);
        if count == 0 {
            st.set_playlist_selected(0);
        } else if st.playlist_selected >= count {
            st.set_playlist_selected(count - 1);
        }

        let current_uri = st.current_track_uri.clone();
        let current_entry = downloaded
            .iter()
            .find(|entry| entry.uri == current_uri)
            .cloned();
        let current_still_downloaded = current_entry.is_some();

        if !player_active && st.mode != AppMode::Spotify {
            let restore_entry = if current_still_downloaded {
                current_entry.clone()
            } else {
                select_local_restore_target(&downloaded, persisted_last_local_uri.as_deref())
                    .cloned()
            };

            if let Some(entry) = restore_entry {
                if current_uri.is_empty() || !current_still_downloaded {
                    st.set_mode(AppMode::Local);
                    st.set_paused(true);
                    st.current_track_uri = entry.uri.clone();
                    st.track_name = entry.name.clone();
                    st.artist_name = entry.artist.clone();
                    st.album_name = entry.album.clone();
                    st.set_duration(entry.duration_ms.unwrap_or(0));
                    st.set_position(0, Instant::now());
                    st.set_favorited(true);
                    seed_entry = Some(entry.clone());
                } else if st.mode == AppMode::Local {
                    seed_entry = current_entry;
                }
            } else {
                st.set_mode(AppMode::Waiting);
                st.current_track_uri.clear();
                st.track_name.clear();
                st.artist_name.clear();
                st.album_name.clear();
                st.set_duration(0);
                st.set_position(0, Instant::now());
                st.set_favorited(false);
                clear_cover = true;
            }
        }
    }

    if let Some(entry) = seed_entry {
        load_local_cover(&entry, render_state);
    } else if clear_cover {
        network::update_cover(None, render_state);
    }
}

fn current_local_volume_percent(app_state: &Arc<Mutex<AppState>>) -> u32 {
    let st = app_state.lock().unwrap();
    local_volume_percent(st.volume, st.volume_max)
}

fn adjust_local_volume(
    app_state: &Arc<Mutex<AppState>>,
    local_player: &Arc<Mutex<LocalPlayer>>,
    spotify_audio: &Arc<SpotifyPipeAudio>,
    delta: i32,
) {
    let percent = {
        let mut st = app_state.lock().unwrap();
        let volume_max = st.volume_max.max(1);
        let next_volume = (st.volume + delta).clamp(0, volume_max);
        st.set_volume(next_volume, volume_max);
        local_volume_percent(st.volume, st.volume_max)
    };
    local_player.lock().unwrap().set_volume_percent(percent);
    spotify_audio.set_volume_percent(percent);
}

/// Sync local player's current track info into AppState for rendering.
fn sync_local_track_to_app(
    player: &LocalPlayer,
    app_state: &Arc<Mutex<AppState>>,
    favorites: &Arc<Mutex<FavoritesManager>>,
) {
    if let Some(entry) = player.current_entry() {
        let uri = entry.uri.clone();
        let mut st = app_state.lock().unwrap();
        st.current_track_uri = uri.clone();
        st.track_name = entry.name.clone();
        st.artist_name = entry.artist.clone();
        st.album_name = entry.album.clone();
        st.set_duration(entry.duration_ms.unwrap_or(0));
        st.set_position(player.position_ms(), Instant::now());
        let fav = favorites.lock().unwrap();
        st.set_favorited(fav.is_favorited(&uri));
        drop(fav);
        drop(st);
        remember_last_local_track_uri(&uri);
    }
}

/// Load cover art for a local track.
/// Priority: local jpg file → Spotify cover cache → fetch from URL → clear cover.
fn load_local_cover(entry: &FavoriteEntry, render_state: &Arc<Mutex<RenderState>>) {
    // 1. Try local cover file (downloaded alongside MP3)
    if let Some(ref cover_path) = entry.cover_path {
        if std::path::Path::new(cover_path).exists() {
            if let Ok(data) = std::fs::read(cover_path) {
                if let Some(img) = resources::decode_image_bytes(&data) {
                    let mut rs = render_state.lock().unwrap();
                    let cover_key = cover_path.clone();
                    rs.replace_cover(&cover_key, &img);
                    return;
                }
            }
        }
    }
    // 2. Try Spotify cover URL (uses existing cover cache in /tmp/spotify-ui-cover-cache/)
    if !entry.cover_url.is_empty() {
        network::update_cover(Some(&entry.cover_url), render_state);
    } else {
        // 3. No cover available — clear
        network::update_cover(None, render_state);
    }
}

/// Monitor thread: checks if local playback track ended and updates position.
fn local_playback_monitor(
    app_state: Arc<Mutex<AppState>>,
    render_state: Arc<Mutex<RenderState>>,
    local_player: Arc<Mutex<LocalPlayer>>,
    favorites: Arc<Mutex<FavoritesManager>>,
    pending_removals: Arc<Mutex<HashMap<String, FavoriteEntry>>>,
    quit: Arc<AtomicBool>,
) {
    loop {
        std::thread::sleep(Duration::from_millis(500));
        if quit.load(Ordering::Relaxed) {
            return;
        }

        let current_local_uri = current_local_track_uri(&local_player);
        finalize_pending_removals(&pending_removals, &favorites, current_local_uri.as_deref());

        let mode = app_state.lock().unwrap().mode;
        if mode != AppMode::Local {
            continue;
        }

        let volume_percent = current_local_volume_percent(&app_state);
        let mut player = local_player.lock().unwrap();
        player.set_volume_percent(volume_percent);

        if player.refresh_audio_route() {
            sync_local_track_to_app(&player, &app_state, &favorites);
            continue;
        }

        // Check if track ended and auto-advance
        if player.check_and_advance() {
            sync_local_track_to_app(&player, &app_state, &favorites);
            if let Some(entry) = player.current_entry() {
                let entry = entry.clone();
                drop(player);
                load_local_cover(&entry, &render_state);
            }
            continue;
        }

        // Update position display
        let pos = player.position_ms();
        drop(player);
        let mut st = app_state.lock().unwrap();
        st.set_position(pos, Instant::now());
    }
}

// Global storage for the quit flag pointer (used by signal handler)
static QUIT_FLAG: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

extern "C" fn signal_handler(_sig: libc::c_int) {
    let ptr = QUIT_FLAG.load(Ordering::SeqCst);
    if ptr != 0 {
        let flag = unsafe { &*(ptr as *const AtomicBool) };
        flag.store(true, Ordering::Relaxed);
    }
}

fn autostart_local_playback_enabled(value: Option<&str>) -> bool {
    value
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

fn is_startup_suppressible_input_action(action: InputAction) -> bool {
    matches!(
        action,
        InputAction::ToggleFavorite
            | InputAction::TogglePlayPause
            | InputAction::NextTrack
            | InputAction::PrevTrack
            | InputAction::VolumeUp
            | InputAction::VolumeDown
            | InputAction::StartLocalPlayback
            | InputAction::StopLocalPlayback
            | InputAction::TogglePlaylist
            | InputAction::PlaylistUp
            | InputAction::PlaylistDown
            | InputAction::PlaylistSelect
            | InputAction::PlaylistDelete
            | InputAction::LockScreen
            | InputAction::UnlockScreen
            | InputAction::RequestExit
            | InputAction::ExitApp
    )
}

fn is_startup_launch_edge_action(action: InputAction) -> bool {
    matches!(
        action,
        InputAction::TogglePlayPause
            | InputAction::StartLocalPlayback
            | InputAction::RequestExit
            | InputAction::ExitApp
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::favorites::FavoriteSource;

    fn test_entry(uri: &str) -> FavoriteEntry {
        FavoriteEntry {
            uri: uri.to_string(),
            name: "Track".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            cover_url: String::new(),
            source: FavoriteSource::Spotify,
            file_path: Some(format!("/tmp/{uri}.mp3")),
            cover_path: None,
            duration_ms: Some(1_000),
            spotify_duration_ms: None,
            downloaded: true,
            added_at: "0".to_string(),
        }
    }

    #[test]
    fn current_local_track_removal_is_the_only_case_that_defers_file_deletion() {
        assert!(should_defer_favorite_file_deletion(
            Some("track:1"),
            "track:1"
        ));
        assert!(!should_defer_favorite_file_deletion(
            Some("track:1"),
            "track:2"
        ));
        assert!(!should_defer_favorite_file_deletion(None, "track:1"));
    }

    #[test]
    fn pending_removals_finalize_after_track_changes_away() {
        let mut pending = HashMap::new();
        pending.insert("track:1".to_string(), test_entry("track:1"));
        pending.insert("track:2".to_string(), test_entry("track:2"));

        let mut ready_while_track_one_is_current =
            pending_removal_uris_ready_for_finalization(&pending, Some("track:1"));
        ready_while_track_one_is_current.sort();
        assert_eq!(
            ready_while_track_one_is_current,
            vec!["track:2".to_string()]
        );

        let mut ready_with_no_current = pending_removal_uris_ready_for_finalization(&pending, None);
        ready_with_no_current.sort();
        assert_eq!(
            ready_with_no_current,
            vec!["track:1".to_string(), "track:2".to_string()]
        );
    }

    #[test]
    fn cleanup_orphaned_files_skips_when_favorites_failed_to_load() {
        let dir = std::env::temp_dir().join(format!(
            "sideb-cleanup-corrupt-favorites-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let music_dir = dir.join("music");
        std::fs::create_dir_all(&music_dir).unwrap();
        let favorites_path = dir.join("favorites.json");
        let orphan = music_dir.join("orphan.mp3");
        std::fs::write(&favorites_path, b"{not valid json").unwrap();
        std::fs::write(&orphan, b"mp3").unwrap();

        let favorites = Arc::new(Mutex::new(FavoritesManager::load(&favorites_path)));

        cleanup_orphaned_files_in_dir(&favorites, &music_dir);

        assert!(orphan.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unavailable_playlist_entry_sets_user_notice() {
        let app_state = Arc::new(Mutex::new(AppState::new()));
        let mut entry = test_entry("track:queued");
        entry.downloaded = false;
        entry.file_path = None;

        show_playlist_unavailable_notice(&app_state, &entry);

        assert_eq!(
            app_state
                .lock()
                .unwrap()
                .active_notice_message(Instant::now()),
            Some("Download first".to_string())
        );
    }

    #[test]
    fn import_finish_with_failures_shows_notice() {
        let app_state = Arc::new(Mutex::new(AppState::new()));
        app_state.lock().unwrap().set_import_progress(1, 2);

        finish_import_progress(&app_state, 1);

        let mut st = app_state.lock().unwrap();
        assert_eq!(st.import_progress, None);
        assert_eq!(
            st.active_notice_message(Instant::now()),
            Some("Import failed".to_string())
        );
    }

    #[test]
    fn playlist_selection_wraps_up_from_first_item() {
        assert_eq!(advance_playlist_selection(0, 4, PlaylistMove::Up), 3);
    }

    #[test]
    fn playlist_selection_wraps_down_from_last_item() {
        assert_eq!(advance_playlist_selection(3, 4, PlaylistMove::Down), 0);
    }

    #[test]
    fn playlist_selection_stays_zero_when_list_is_empty() {
        assert_eq!(advance_playlist_selection(0, 0, PlaylistMove::Up), 0);
        assert_eq!(advance_playlist_selection(0, 0, PlaylistMove::Down), 0);
    }

    #[test]
    fn autostart_local_playback_flag_requires_truthy_value() {
        assert!(autostart_local_playback_enabled(Some("1")));
        assert!(autostart_local_playback_enabled(Some("true")));
        assert!(autostart_local_playback_enabled(Some("yes")));
        assert!(!autostart_local_playback_enabled(Some("0")));
        assert!(!autostart_local_playback_enabled(Some("")));
        assert!(!autostart_local_playback_enabled(None));
    }

    #[test]
    fn startup_command_guard_blocks_hardware_playback_actions_during_hard_window() {
        let now = Instant::now();
        let mut guard = StartupCommandGuard::new(now);

        assert!(guard.should_suppress(InputAction::TogglePlayPause, now));
        assert!(guard.should_suppress(
            InputAction::StartLocalPlayback,
            now + Duration::from_millis(5500)
        ));
        assert!(!guard.should_suppress(
            InputAction::SpotifyActivated,
            now + Duration::from_millis(5600)
        ));
        assert!(!guard.should_suppress(
            InputAction::ImportFinished { failed: 0 },
            now + Duration::from_millis(5700)
        ));
    }

    #[test]
    fn startup_command_suppression_blocks_delayed_frontend_launch_burst() {
        let now = Instant::now();
        let mut guard = StartupCommandGuard::new(now);

        assert!(guard.should_suppress(
            InputAction::TogglePlayPause,
            now + Duration::from_millis(8500)
        ));
        assert!(guard.should_suppress(
            InputAction::RequestExit,
            now + Duration::from_millis(10_800)
        ));
        assert!(guard.should_suppress(
            InputAction::RequestExit,
            now + Duration::from_millis(11_200)
        ));
        assert!(!guard.should_suppress(
            InputAction::TogglePlayPause,
            now + Duration::from_millis(13_000)
        ));
    }

    #[test]
    fn startup_command_suppression_swallows_first_late_launch_edge_action() {
        let now = Instant::now();
        let mut guard = StartupCommandGuard::new(now);

        assert!(guard.should_suppress(InputAction::TogglePlayPause, now + Duration::from_secs(14)));
        assert!(guard.should_suppress(
            InputAction::RequestExit,
            now + Duration::from_millis(14_500)
        ));
        assert!(!guard.should_suppress(InputAction::TogglePlayPause, now + Duration::from_secs(17)));
    }

    #[test]
    fn local_restore_target_prefers_preempted_uri() {
        let downloaded = vec![test_entry("track:1"), test_entry("track:2")];
        let target = select_local_restore_target(&downloaded, Some("track:2"))
            .map(|entry| entry.uri.clone());
        assert_eq!(target.as_deref(), Some("track:2"));
    }

    #[test]
    fn local_restore_target_falls_back_to_first_downloaded() {
        let downloaded = vec![test_entry("track:1"), test_entry("track:2")];
        let target = select_local_restore_target(&downloaded, Some("missing"))
            .map(|entry| entry.uri.clone());
        assert_eq!(target.as_deref(), Some("track:1"));
    }

    #[test]
    fn spotify_stop_sleep_requires_prior_active_session_and_no_downloads() {
        assert!(!should_enable_stop_sleep_after_spotify_deactivated(
            false, false
        ));
        assert!(!should_enable_stop_sleep_after_spotify_deactivated(
            true, true
        ));
        assert!(should_enable_stop_sleep_after_spotify_deactivated(
            true, false
        ));
    }

    #[test]
    fn local_restore_target_is_none_without_downloads() {
        assert!(select_local_restore_target(&[], Some("track:1")).is_none());
    }

    #[test]
    fn local_restore_target_falls_back_to_first_downloaded_without_remembered_uri() {
        let downloaded = vec![test_entry("track:1")];
        let target = select_local_restore_target(&downloaded, None).map(|entry| entry.uri.clone());
        assert_eq!(target.as_deref(), Some("track:1"));
    }

    #[test]
    fn playback_state_round_trips_last_local_track_uri() {
        let dir = temp_test_dir("playback-state-roundtrip");
        let path = dir.join("playback_state.json");
        let state = PlaybackState {
            last_local_track_uri: Some("track:remembered".to_string()),
        };

        save_playback_state_to(&path, &state);

        assert_eq!(load_playback_state_from(&path), state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn playback_state_parse_error_falls_back_to_default() {
        let dir = temp_test_dir("playback-state-corrupt");
        let path = dir.join("playback_state.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, b"{not-json").unwrap();

        assert_eq!(load_playback_state_from(&path), PlaybackState::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp_test_dir(prefix: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "sideb-main-{prefix}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir
    }
}
