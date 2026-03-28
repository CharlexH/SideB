use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::app::AppState;
use crate::constants::*;
use crate::mode::{AppMode, InputAction};
use crate::network;
use crate::types::InputEvent;

/// Parse a raw 24-byte Linux input_event struct (aarch64 layout).
/// Layout: timeval (16 bytes) + type (u16) + code (u16) + value (i32)
fn parse_input_event(buf: &[u8; 24]) -> InputEvent {
    InputEvent {
        event_type: u16::from_le_bytes([buf[16], buf[17]]),
        code: u16::from_le_bytes([buf[18], buf[19]]),
        value: i32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]),
    }
}

/// Read input events from multiple evdev devices.
pub fn run(state: Arc<Mutex<AppState>>, quit: Arc<AtomicBool>, cmd_tx: Sender<InputAction>) {
    let paths = ["/dev/input/event3", "/dev/input/event0"];
    let mut handles = Vec::new();

    for path in &paths {
        let state = Arc::clone(&state);
        let quit = Arc::clone(&quit);
        let cmd_tx = cmd_tx.clone();
        let path = path.to_string();

        let handle = std::thread::spawn(move || {
            read_input_device(&path, state, quit, cmd_tx);
        });
        handles.push(handle);
    }

    for h in handles {
        let _ = h.join();
    }
}

fn read_input_device(
    path: &str,
    state: Arc<Mutex<AppState>>,
    quit: Arc<AtomicBool>,
    cmd_tx: Sender<InputAction>,
) {
    // Retry open up to 5 times
    let mut file = None;
    for attempt in 1..=5 {
        match File::open(path) {
            Ok(f) => {
                file = Some(f);
                break;
            }
            Err(e) => {
                eprintln!("input open {path} failed (attempt {attempt}): {e}");
                if quit.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }

    let mut file = match file {
        Some(f) => f,
        None => {
            eprintln!("giving up on input device {path}");
            return;
        }
    };

    eprintln!("reading input from {path}");
    let mut buf = [0u8; 24];

    loop {
        if quit.load(Ordering::Relaxed) {
            return;
        }

        if let Err(e) = file.read_exact(&mut buf) {
            eprintln!("input read error: {e}");
            return;
        }

        let ev = parse_input_event(&buf);

        // Only handle key-down events (value == 1) and D-pad
        if ev.event_type == EV_KEY && ev.value != 1 {
            continue;
        }

        // MENU always exits
        if ev.event_type == EV_KEY && ev.code == KEY_MENU {
            eprintln!("exit requested via MENU");
            let _ = cmd_tx.send(InputAction::ExitApp);
            quit.store(true, Ordering::Relaxed);
            return;
        }

        // Read current mode and playlist visibility
        let (mode, playlist_visible) = {
            let st = state.lock().unwrap();
            (st.mode, st.playlist_visible)
        };

        // Route based on whether playlist overlay is visible
        if playlist_visible {
            handle_playlist_input(&ev, &cmd_tx, &quit);
        } else {
            handle_normal_input(&ev, mode, &state, &cmd_tx, &quit);
        }
    }
}

/// Handle input when the playlist overlay is showing.
fn handle_playlist_input(
    ev: &InputEvent,
    cmd_tx: &Sender<InputAction>,
    _quit: &Arc<AtomicBool>,
) {
    if ev.event_type == EV_KEY {
        match ev.code {
            BTN_B => {
                let _ = cmd_tx.send(InputAction::TogglePlaylist);
            }
            BTN_A => {
                let _ = cmd_tx.send(InputAction::PlaylistSelect);
            }
            BTN_X => {
                let _ = cmd_tx.send(InputAction::PlaylistDelete);
            }
            BTN_Y => {
                let _ = cmd_tx.send(InputAction::TogglePlaylist);
            }
            _ => {}
        }
    } else if ev.event_type == EV_ABS {
        match ev.code {
            ABS_HAT0Y => {
                if ev.value < 0 {
                    let _ = cmd_tx.send(InputAction::PlaylistUp);
                } else if ev.value > 0 {
                    let _ = cmd_tx.send(InputAction::PlaylistDown);
                }
            }
            _ => {}
        }
    }
}

/// Handle input in normal (non-overlay) mode.
fn handle_normal_input(
    ev: &InputEvent,
    mode: AppMode,
    state: &Arc<Mutex<AppState>>,
    cmd_tx: &Sender<InputAction>,
    quit: &Arc<AtomicBool>,
) {
    if ev.event_type == EV_KEY {
        match ev.code {
            BTN_A => {
                // Debounce
                let should_act = {
                    let mut st = state.lock().unwrap();
                    let since = st.last_action.elapsed().as_millis();
                    if since > DEBOUNCE_MS {
                        st.last_action = Instant::now();
                        true
                    } else {
                        false
                    }
                };
                if !should_act {
                    return;
                }

                match mode {
                    AppMode::Waiting => {
                        let _ = cmd_tx.send(InputAction::StartLocalPlayback);
                    }
                    AppMode::Spotify => {
                        // Direct Spotify API call for low latency
                        let paused = state.lock().unwrap().paused;
                        if paused {
                            network::api_post("/player/resume");
                        } else {
                            network::api_post("/player/pause");
                        }
                    }
                    AppMode::Local => {
                        let _ = cmd_tx.send(InputAction::TogglePlayPause);
                    }
                }
            }

            BTN_B => {
                eprintln!("exit requested via B");
                let _ = cmd_tx.send(InputAction::ExitApp);
                quit.store(true, Ordering::Relaxed);
            },

            BTN_X => {
                if mode != AppMode::Waiting {
                    let _ = cmd_tx.send(InputAction::ToggleFavorite);
                }
            }

            BTN_Y => {
                let _ = cmd_tx.send(InputAction::TogglePlaylist);
            }

            _ => {}
        }
    } else if ev.event_type == EV_ABS {
        match ev.code {
            ABS_HAT0X => {
                if ev.value < 0 {
                    match mode {
                        AppMode::Spotify => network::api_post("/player/prev"),
                        AppMode::Local => {
                            let _ = cmd_tx.send(InputAction::PrevTrack);
                        }
                        _ => {}
                    }
                } else if ev.value > 0 {
                    match mode {
                        AppMode::Spotify => network::api_post("/player/next"),
                        AppMode::Local => {
                            let _ = cmd_tx.send(InputAction::NextTrack);
                        }
                        _ => {}
                    }
                }
            }
            ABS_HAT0Y => {
                if ev.value < 0 {
                    match mode {
                        AppMode::Spotify => network::api_post_volume(5),
                        AppMode::Local => {
                            let _ = cmd_tx.send(InputAction::VolumeUp);
                        }
                        _ => {}
                    }
                } else if ev.value > 0 {
                    match mode {
                        AppMode::Spotify => network::api_post_volume(-5),
                        AppMode::Local => {
                            let _ = cmd_tx.send(InputAction::VolumeDown);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}
