use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::app::AppState;
use crate::constants::*;
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
pub fn run(state: Arc<Mutex<AppState>>, quit: Arc<AtomicBool>) {
    let paths = ["/dev/input/event3", "/dev/input/event0"];
    let mut handles = Vec::new();

    for path in &paths {
        let state = Arc::clone(&state);
        let quit = Arc::clone(&quit);
        let path = path.to_string();

        let handle = std::thread::spawn(move || {
            read_input_device(&path, state, quit);
        });
        handles.push(handle);
    }

    for h in handles {
        let _ = h.join();
    }
}

fn read_input_device(path: &str, state: Arc<Mutex<AppState>>, quit: Arc<AtomicBool>) {
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

        // B and MENU always exit
        if ev.event_type == EV_KEY && ev.value == 1 {
            if ev.code == BTN_B || ev.code == KEY_MENU {
                eprintln!("exit requested via button");
                quit.store(true, Ordering::Relaxed);
                return;
            }
        }

        // A button: play/pause with debounce
        if ev.event_type == EV_KEY && ev.value == 1 && ev.code == BTN_A {
            let should_toggle = {
                let mut st = state.lock().unwrap();
                let since = st.last_action.elapsed().as_millis();
                if since > DEBOUNCE_MS {
                    st.last_action = Instant::now();
                    true
                } else {
                    eprintln!("debounce: ignored A press ({since}ms)");
                    false
                }
            };
            if should_toggle {
                let paused = state.lock().unwrap().paused;
                if paused {
                    eprintln!("action: resume");
                    network::api_post("/player/resume");
                } else {
                    eprintln!("action: pause");
                    network::api_post("/player/pause");
                }
            }
        }

        // D-pad
        if ev.event_type == EV_ABS {
            match ev.code {
                ABS_HAT0X => {
                    if ev.value < 0 {
                        network::api_post("/player/prev");
                    } else if ev.value > 0 {
                        network::api_post("/player/next");
                    }
                }
                ABS_HAT0Y => {
                    if ev.value < 0 {
                        network::api_post_volume(5);
                    } else if ev.value > 0 {
                        network::api_post_volume(-5);
                    }
                }
                _ => {}
            }
        }
    }
}
