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
mod local_import;
mod local_player;
mod log_utils;
mod mode;
mod network;
mod paths;
mod playlist_view;
mod render;
mod resources;
mod types;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use app::{AppState, Assets};
use constants::*;
use download::{DownloadManager, DownloadRequest};
use favorites::{FavoriteEntry, FavoriteSource, FavoritesManager};
use font::FontSet;
use framebuffer::Framebuffer;
use local_player::LocalPlayer;
use mode::{AppMode, InputAction};
use paths::{app_paths, detect_paths, init_paths};
use render::RenderState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaylistMove {
    Up,
    Down,
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

    let imported = local_import::scan_once(&favorites);
    if imported > 0 {
        eprintln!("import: startup imported {imported} local track(s)");
    }

    // Clean up orphaned files in music directory
    cleanup_orphaned_files(&favorites);

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
                cmd_pending_removals,
                download_manager,
                cmd_quit,
            );
        })
        .expect("spawn command processor thread");

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
    pending_removals: Arc<Mutex<HashMap<String, FavoriteEntry>>>,
    download_manager: DownloadManager,
    quit: Arc<AtomicBool>,
) {
    for action in rx.iter() {
        if quit.load(Ordering::Relaxed) {
            return;
        }

        match action {
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
                        });
                    }
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
                let downloaded = favorites.lock().unwrap().downloaded_entries();
                let mut player = local_player.lock().unwrap();
                player.refresh_playlist(downloaded);
                player.next();
                sync_local_track_to_app(&player, &app_state, &favorites);
                let entry = player.current_entry().cloned();
                drop(player);
                if let Some(entry) = entry {
                    load_local_cover(&entry, &render_state);
                }
            }

            InputAction::PrevTrack => {
                let downloaded = favorites.lock().unwrap().downloaded_entries();
                let mut player = local_player.lock().unwrap();
                player.refresh_playlist(downloaded);
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

            InputAction::SpotifyActivated => {
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
                let st = app_state.lock().unwrap();
                let uri = st.current_track_uri.clone();
                drop(st);
                let is_fav = favorites.lock().unwrap().is_favorited(&uri);
                app_state.lock().unwrap().set_favorited(is_fav);
            }

            InputAction::SpotifyDeactivated => {
                let remembered_uri = app_state
                    .lock()
                    .unwrap()
                    .spotify_preempted_local_uri
                    .clone();
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
                    drop(st);
                    load_local_cover(&entry, &render_state);
                    eprintln!(
                        "cmd: Spotify deactivated, restored paused local track {}",
                        entry.uri
                    );
                } else {
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

fn select_local_restore_target<'a>(
    downloaded: &'a [FavoriteEntry],
    remembered_uri: Option<&str>,
) -> Option<&'a FavoriteEntry> {
    let remembered_uri = remembered_uri?;
    downloaded
        .iter()
        .find(|entry| entry.uri == remembered_uri)
        .or_else(|| downloaded.first())
}

/// Remove orphaned files from the music directory that are not referenced by any favorite.
fn cleanup_orphaned_files(favorites: &Arc<Mutex<FavoritesManager>>) {
    let fav = favorites.lock().unwrap();
    let referenced = fav.referenced_files();
    drop(fav);

    let music_dir = app_paths().music_dir.clone();
    let entries = match std::fs::read_dir(&music_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    let mut removed = 0u32;
    for entry in entries.flatten() {
        let path = entry.path();
        let path_str = path.to_string_lossy().to_string();

        // Only clean up .mp3 and .jpg files
        match path.extension().and_then(|e| e.to_str()) {
            Some("mp3") | Some("jpg") | Some("jpeg") | Some("png") => {}
            _ => continue,
        }

        if !referenced.contains(&path_str) {
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
        let current_still_downloaded = downloaded.iter().any(|entry| entry.uri == current_uri);

        if !player_active && st.mode != AppMode::Spotify {
            if let Some(entry) = downloaded.first() {
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
    fn local_restore_target_is_none_without_downloads() {
        assert!(select_local_restore_target(&[], Some("track:1")).is_none());
    }

    #[test]
    fn local_restore_target_is_none_without_remembered_uri() {
        let downloaded = vec![test_entry("track:1")];
        assert!(select_local_restore_target(&downloaded, None).is_none());
    }
}
