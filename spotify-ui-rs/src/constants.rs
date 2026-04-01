use std::time::Duration;

// Screen
pub const SCREEN_W: usize = 1024;
pub const SCREEN_H: usize = 768;
pub const BPP: usize = 4; // 32-bit BGRA
pub const FB_SIZE: usize = SCREEN_W * SCREEN_H * BPP;

// Animation
pub const BASE_ANIM_FPS: u64 = 30;
pub const MAX_ANIM_FPS: u64 = 30;
pub const ANIM_FPS: u64 = BASE_ANIM_FPS;
pub const ROTATION_FRAME_COUNT: usize = 30;
pub const TAPEROLL_FRAME_COUNT: usize = 30;
pub const TAPEROLL_SIZE_STEP: i32 = 12;
pub const WHEEL_ROTATION_PERIOD: Duration = Duration::from_secs(2);
pub const SOUNDWAVE_TARGET_REFRESH: Duration = Duration::from_millis(66);
pub const SOUNDWAVE_EASE: f64 = 0.35;
pub const SOUNDWAVE_IDLE_EASE: f64 = 0.20;
pub const SOUNDWAVE_MIN_HEIGHT: f64 = 8.0;
pub const SOUNDWAVE_MAX_HEIGHT: f64 = 36.0;

// Cassette layout
pub const TAPE_BASE_X: i32 = 16;
pub const TAPE_BASE_Y: i32 = 28;
pub const WINDOW_X: i32 = 68;
pub const WINDOW_Y: i32 = 68;
pub const COVER_X: i32 = 68;
pub const COVER_Y: i32 = 68;
pub const WINDOW_W: usize = 888;
pub const WINDOW_H: usize = 384;

// Taperoll
pub const LEFT_ROLL_CENTER_X: i32 = 308;
pub const RIGHT_ROLL_CENTER_X: i32 = 716;
pub const ROLL_CENTER_Y: i32 = 292;
pub const LEFT_ROLL_MIN_SIZE: i32 = 200;
pub const LEFT_ROLL_MAX_SIZE: i32 = 432;
pub const RIGHT_ROLL_MIN_SIZE: i32 = 200;
pub const RIGHT_ROLL_MAX_SIZE: i32 = 432;

// Wheels
pub const LEFT_WHEEL_X: i32 = 248;
pub const LEFT_WHEEL_Y: i32 = 232;
pub const RIGHT_WHEEL_X: i32 = 656;
pub const RIGHT_WHEEL_Y: i32 = 232;

// Status display
pub const STATUS_BASELINE_Y: i32 = 677;
pub const HINTS_BASELINE_Y: i32 = 736;

// Bottom bar icons (all 32x32)
pub const BAR_ICON_SIZE: i32 = 32;
pub const BAR_ICON_Y: i32 = 651;
pub const SPOTIFY_ICON_X: i32 = 20;
pub const FAV_ICON_X: i32 = 68;
pub const PLAY_ICON_MARGIN: i32 = 8;

// API
pub const API_BASE: &str = "http://127.0.0.1:3678";

// Input event constants
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;
pub const BTN_A: u16 = 305;
pub const BTN_B: u16 = 304;
pub const BTN_X: u16 = 308;
pub const BTN_Y: u16 = 307;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;
pub const BTN_START: u16 = 315;
pub const KEY_MENU: u16 = 139;

// Debounce
pub const DEBOUNCE_MS: u128 = 500;

// Playlist overlay
pub const PLAYLIST_MARGIN: i32 = 48;
pub const PLAYLIST_X: i32 = PLAYLIST_MARGIN;
pub const PLAYLIST_Y: i32 = PLAYLIST_MARGIN;
pub const PLAYLIST_W: i32 = SCREEN_W as i32 - 2 * PLAYLIST_MARGIN;
pub const PLAYLIST_H: i32 = SCREEN_H as i32 - 2 * PLAYLIST_MARGIN;
pub const PLAYLIST_ITEM_HEIGHT: i32 = 48;
pub const PLAYLIST_HEADER_HEIGHT: i32 = 56;
pub const PLAYLIST_FOOTER_HEIGHT: i32 = 40;
pub const PLAYLIST_VISIBLE_ITEMS: usize = 12;

pub const YTDLP_BIN: &str = "/tmp/yt-dlp";
pub const FFMPEG_TRANSCODER_BIN: &str = "/tmp/ffmpeg-lite";
pub const SYSTEM_FFMPEG_BIN: &str = "/usr/bin/ffmpeg";

#[cfg(test)]
mod tests {
    use super::{FFMPEG_TRANSCODER_BIN, SYSTEM_FFMPEG_BIN};

    #[test]
    fn bundled_ffmpeg_uses_lite_name() {
        assert_eq!(FFMPEG_TRANSCODER_BIN, "/tmp/ffmpeg-lite");
    }

    #[test]
    fn system_ffmpeg_remains_device_binary() {
        assert_eq!(SYSTEM_FFMPEG_BIN, "/usr/bin/ffmpeg");
    }
}
