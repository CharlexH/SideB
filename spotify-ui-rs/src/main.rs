#![allow(dead_code)]

mod animation;
mod app;
mod constants;
mod drawing;
mod font;
mod framebuffer;
mod image_ops;
mod input;
mod network;
mod render;
mod resources;
mod types;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use app::{AppState, Assets};
use constants::FB_SIZE;
use font::FontSet;
use framebuffer::Framebuffer;
use render::RenderState;

fn main() {
    eprintln!("spotify-ui (rust) starting");

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
        &fonts,
    );
    eprintln!("render caches ready");

    let app_state = Arc::new(Mutex::new(AppState::new()));
    let render_state = Arc::new(Mutex::new(render_state));
    let quit = Arc::new(AtomicBool::new(false));

    // Allocate back buffer
    let mut back_buf = vec![0u8; FB_SIZE];

    // Initial render (waiting screen)
    {
        let rs = render_state.lock().unwrap();
        back_buf.copy_from_slice(&rs.scene_waiting);
        fb.swap_buffers(&back_buf);
    }

    // Spawn input thread
    let input_state = Arc::clone(&app_state);
    let input_quit = Arc::clone(&quit);
    let _input_handle = std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            input::run(input_state, input_quit);
        })
        .expect("spawn input thread");

    // Spawn WebSocket thread
    let ws_state = Arc::clone(&app_state);
    let ws_render = Arc::clone(&render_state);
    let ws_quit = Arc::clone(&quit);
    let _ws_handle = std::thread::Builder::new()
        .name("websocket".into())
        .spawn(move || {
            network::listen_events(ws_state, ws_render, ws_quit);
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

    // Run render loop on main thread
    // (We can't move fonts to another thread easily since FontSet isn't Send in
    //  some configurations, so we keep it on the main thread)
    let render_quit = Arc::clone(&quit);

    // Set up signal handler
    let sig_quit = Arc::clone(&quit);
    let _ = std::thread::Builder::new()
        .name("signal".into())
        .spawn(move || {
            // Block on SIGINT/SIGTERM using a simple polling approach
            // since we can't use signal-hook with musl easily
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
        // We use a global atomic since signal handlers can't capture closures
        QUIT_FLAG.store(quit_for_signal.as_ref() as *const AtomicBool as usize, Ordering::SeqCst);

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
    );

    // Clear screen on exit
    for byte in back_buf.iter_mut() {
        *byte = 0;
    }
    fb.swap_buffers(&back_buf);

    eprintln!("exiting");

    // Input/WebSocket threads use blocking reads that can't be interrupted
    // by the quit flag alone. Force-exit like Go does (goroutines die with main).
    std::process::exit(0);
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
