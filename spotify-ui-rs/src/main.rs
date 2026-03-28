#![allow(dead_code)]

mod animation;
mod app;
mod constants;
mod download;
mod drawing;
mod favorites;
mod font;
mod framebuffer;
mod image_ops;
mod input;
mod local_player;
mod mode;
mod network;
mod playlist_view;
mod render;
mod resources;
mod types;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use app::{AppState, Assets};
use constants::*;
use download::{DownloadManager, DownloadRequest};
use favorites::{FavoriteEntry, FavoritesManager};
use font::FontSet;
use framebuffer::Framebuffer;
use local_player::LocalPlayer;
use mode::{AppMode, InputAction};
use render::RenderState;

fn main() {
    eprintln!("sideb starting");

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
        &fonts,
    );
    eprintln!("render caches ready");

    // Ensure data directories exist
    let _ = std::fs::create_dir_all(MUSIC_DIR);

    let app_state = Arc::new(Mutex::new(AppState::new()));
    let render_state = Arc::new(Mutex::new(render_state));
    let quit = Arc::new(AtomicBool::new(false));

    // Initialize favorites and local player
    let favorites = Arc::new(Mutex::new(FavoritesManager::load(FAVORITES_PATH)));
    let local_player = Arc::new(Mutex::new(LocalPlayer::new()));

    // Create command channel
    let (cmd_tx, cmd_rx) = mpsc::channel::<InputAction>();

    // Create download manager (spawns its own background thread)
    let download_manager = DownloadManager::new(Arc::clone(&favorites));

    // Set initial mode: Local (paused) if favorites exist, else Waiting
    {
        let fav = favorites.lock().unwrap();
        let downloaded = fav.downloaded_entries();
        if !downloaded.is_empty() {
            let entry = downloaded[0].clone();
            let mut st = app_state.lock().unwrap();
            st.set_mode(AppMode::Local);
            st.set_paused(true);
            st.track_name = entry.name.clone();
            st.artist_name = entry.artist.clone();
            st.current_track_uri = entry.uri.clone();
            st.duration = entry.duration_ms.unwrap_or(0);
            st.position = 0;
            st.set_favorited(true);
            drop(st);
            // Load cover art for first track
            load_local_cover(&entry, &render_state);
        }
    }

    // Allocate back buffer
    let mut back_buf = vec![0u8; FB_SIZE];

    // Initial render
    {
        let st = app_state.lock().unwrap();
        let rs = render_state.lock().unwrap();
        if st.mode == AppMode::Waiting {
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
    let _poll_handle = std::thread::Builder::new()
        .name("status-poll".into())
        .spawn(move || {
            network::poll_status(poll_state, poll_render, poll_quit);
        })
        .expect("spawn status poll thread");

    // Spawn command processor thread
    let cmd_app_state = Arc::clone(&app_state);
    let cmd_render_state = Arc::clone(&render_state);
    let cmd_favorites = Arc::clone(&favorites);
    let cmd_local_player = Arc::clone(&local_player);
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
                download_manager,
                cmd_quit,
            );
        })
        .expect("spawn command processor thread");

    // Spawn local playback monitor thread
    let mon_app_state = Arc::clone(&app_state);
    let mon_render_state = Arc::clone(&render_state);
    let mon_local_player = Arc::clone(&local_player);
    let mon_favorites = Arc::clone(&favorites);
    let mon_quit = Arc::clone(&quit);
    let _mon_handle = std::thread::Builder::new()
        .name("local-monitor".into())
        .spawn(move || {
            local_playback_monitor(
                mon_app_state,
                mon_render_state,
                mon_local_player,
                mon_favorites,
                mon_quit,
            );
        })
        .expect("spawn local monitor thread");

    // Run render loop on main thread
    let render_quit = Arc::clone(&quit);

    // Set up signal handler
    let sig_quit = Arc::clone(&quit);
    let _ = std::thread::Builder::new()
        .name("signal".into())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if sig_quit.load(Ordering::Relaxed) {
                    return;
                }
            }
        });

    // Install signal handlers via libc
    unsafe {
        let quit_for_signal = Arc::clone(&quit);
        QUIT_FLAG
            .store(quit_for_signal.as_ref() as *const AtomicBool as usize, Ordering::SeqCst);

        libc::signal(libc::SIGINT, signal_handler as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, signal_handler as *const () as libc::sighandler_t);
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
    );

    // Stop all playback on exit
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
fn command_processor(
    rx: mpsc::Receiver<InputAction>,
    app_state: Arc<Mutex<AppState>>,
    render_state: Arc<Mutex<RenderState>>,
    favorites: Arc<Mutex<FavoritesManager>>,
    local_player: Arc<Mutex<LocalPlayer>>,
    download_manager: DownloadManager,
    quit: Arc<AtomicBool>,
) {
    for action in rx.iter() {
        if quit.load(Ordering::Relaxed) {
            return;
        }

        match action {
            InputAction::ExitApp => {
                quit.store(true, Ordering::Relaxed);
                return;
            }

            InputAction::ToggleFavorite => {
                let (uri, name, artist, album, cover_url) = {
                    let st = app_state.lock().unwrap();
                    match st.mode {
                        AppMode::Spotify => {
                            let cover = render_state
                                .lock()
                                .unwrap()
                                .requested_cover_url
                                .clone()
                                .unwrap_or_default();
                            (
                                st.current_track_uri.clone(),
                                st.track_name.clone(),
                                st.artist_name.clone(),
                                st.album_name.clone(),
                                cover,
                            )
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

                let mut fav = favorites.lock().unwrap();
                if fav.is_favorited(&uri) {
                    // Unfavorite
                    fav.remove(&uri);
                    app_state.lock().unwrap().set_favorited(false);
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
                        file_path: None,
                        cover_path: None,
                        duration_ms: None,
                        downloaded: false,
                        added_at: format!("{}", now),
                    };
                    fav.add(entry);
                    app_state.lock().unwrap().set_favorited(true);

                    // Trigger download
                    download_manager.enqueue(DownloadRequest {
                        uri,
                        track_name: name,
                        artist_name: artist,
                        cover_url,
                    });
                }
            }

            InputAction::TogglePlayPause => {
                let mut player = local_player.lock().unwrap();
                if player.is_active() {
                    player.toggle_pause();
                    let paused = player.is_paused();
                    app_state.lock().unwrap().set_paused(paused);
                } else {
                    // Player not started — start shuffled playback from displayed track
                    network::api_post("/player/pause");
                    let current_uri = app_state.lock().unwrap().current_track_uri.clone();
                    let downloaded = favorites.lock().unwrap().downloaded_entries();
                    if !downloaded.is_empty() {
                        player.start_shuffled_with_first(downloaded, &current_uri);
                        sync_local_track_to_app(&player, &app_state, &favorites);
                        let mut st = app_state.lock().unwrap();
                        st.set_mode(AppMode::Local);
                        st.set_paused(false);
                        drop(st);
                        if let Some(entry) = player.current_entry() {
                            load_local_cover(entry, &render_state);
                        }
                    }
                }
            }

            InputAction::NextTrack => {
                let mut player = local_player.lock().unwrap();
                player.next();
                sync_local_track_to_app(&player, &app_state, &favorites);
                let entry = player.current_entry().cloned();
                drop(player);
                if let Some(entry) = entry {
                    load_local_cover(&entry, &render_state);
                }
            }

            InputAction::PrevTrack => {
                let mut player = local_player.lock().unwrap();
                player.prev();
                sync_local_track_to_app(&player, &app_state, &favorites);
                let entry = player.current_entry().cloned();
                drop(player);
                if let Some(entry) = entry {
                    load_local_cover(&entry, &render_state);
                }
            }

            InputAction::VolumeUp => {
                // For local playback, we still use the system ALSA mixer
                network::api_post_volume(5);
            }

            InputAction::VolumeDown => {
                network::api_post_volume(-5);
            }

            InputAction::StartLocalPlayback => {
                // Pause Spotify first to prevent audio overlap
                network::api_post("/player/pause");

                let downloaded = favorites.lock().unwrap().downloaded_entries();
                if downloaded.is_empty() {
                    eprintln!("cmd: no downloaded tracks for local playback");
                    continue;
                }

                let mut player = local_player.lock().unwrap();
                player.start_shuffled(downloaded);
                sync_local_track_to_app(&player, &app_state, &favorites);

                let mut st = app_state.lock().unwrap();
                st.set_mode(AppMode::Local);
                st.set_paused(false);

                // Load cover for first track
                if let Some(entry) = player.current_entry() {
                    load_local_cover(entry, &render_state);
                }
            }

            InputAction::StopLocalPlayback => {
                let mut player = local_player.lock().unwrap();
                player.stop();
                let mut st = app_state.lock().unwrap();
                st.set_mode(AppMode::Local);
                st.set_paused(true);
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
                if st.playlist_selected > 0 {
                    let new_sel = st.playlist_selected - 1;
                    st.set_playlist_selected(new_sel);
                }
            }

            InputAction::PlaylistDown => {
                let mut st = app_state.lock().unwrap();
                let count = st.playlist_count;
                if count > 0 && st.playlist_selected < count - 1 {
                    let new_sel = st.playlist_selected + 1;
                    st.set_playlist_selected(new_sel);
                }
            }

            InputAction::PlaylistSelect => {
                let selected = app_state.lock().unwrap().playlist_selected;
                let fav = favorites.lock().unwrap();
                let entries = fav.all_entries();
                if selected < entries.len() {
                    let entry = entries[selected].clone();
                    drop(fav);

                    if entry.downloaded && entry.file_path.is_some() {
                        // Pause Spotify first to prevent audio overlap
                        network::api_post("/player/pause");

                        let mut player = local_player.lock().unwrap();
                        // Build a playlist from all downloaded entries
                        let downloaded = favorites.lock().unwrap().downloaded_entries();
                        if player.is_active() {
                            // Just switch to selected track
                            player.play_entry(&entry);
                        } else {
                            // Start fresh with all downloaded, then jump to selected
                            player.start_shuffled(downloaded);
                            player.play_entry(&entry);
                        }
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
                    let mut fav = favorites.lock().unwrap();
                    fav.remove(&uri);
                    let count = fav.count();
                    drop(fav);

                    let mut st = app_state.lock().unwrap();
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
                }
            }

            InputAction::SpotifyActivated => {
                let mut st = app_state.lock().unwrap();
                let was_local = st.mode == AppMode::Local;
                st.set_mode(AppMode::Spotify);

                if was_local {
                    st.local_was_playing = true;
                    drop(st);
                    local_player.lock().unwrap().pause();
                    eprintln!("cmd: Spotify activated, paused local playback");
                } else {
                    st.local_was_playing = false;
                }
            }

            InputAction::SpotifyTrackChanged => {
                let st = app_state.lock().unwrap();
                let uri = st.current_track_uri.clone();
                drop(st);
                let is_fav = favorites.lock().unwrap().is_favorited(&uri);
                app_state.lock().unwrap().set_favorited(is_fav);
            }

            InputAction::SpotifyDeactivated => {
                let mut st = app_state.lock().unwrap();
                if st.local_was_playing {
                    st.set_mode(AppMode::Local);
                    st.set_paused(false);
                    st.local_was_playing = false;
                    drop(st);
                    local_player.lock().unwrap().resume();
                    eprintln!("cmd: Spotify deactivated, resumed local playback");

                    let player = local_player.lock().unwrap();
                    sync_local_track_to_app(&player, &app_state, &favorites);
                    if let Some(entry) = player.current_entry() {
                        load_local_cover(entry, &render_state);
                    }
                } else if st.mode != AppMode::Local {
                    st.set_mode(AppMode::Waiting);
                }
            }
        }
    }
}

/// Sync local player's current track info into AppState for rendering.
fn sync_local_track_to_app(
    player: &LocalPlayer,
    app_state: &Arc<Mutex<AppState>>,
    favorites: &Arc<Mutex<FavoritesManager>>,
) {
    if let Some(entry) = player.current_entry() {
        let mut st = app_state.lock().unwrap();
        st.current_track_uri = entry.uri.clone();
        st.track_name = entry.name.clone();
        st.artist_name = entry.artist.clone();
        st.album_name = entry.album.clone();
        st.set_duration(entry.duration_ms.unwrap_or(0));
        st.set_position(player.position_ms(), Instant::now());
        let fav = favorites.lock().unwrap();
        st.set_favorited(fav.is_favorited(&entry.uri));
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
    quit: Arc<AtomicBool>,
) {
    loop {
        std::thread::sleep(Duration::from_millis(500));
        if quit.load(Ordering::Relaxed) {
            return;
        }

        let mode = app_state.lock().unwrap().mode;
        if mode != AppMode::Local {
            continue;
        }

        let mut player = local_player.lock().unwrap();

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
