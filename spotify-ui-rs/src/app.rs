use std::time::Instant;

use crate::mode::AppMode;
use crate::types::RgbaImage;

/// Mutable application state protected by a Mutex.
pub struct AppState {
    // -- Spotify playback --
    pub current_track_uri: String,
    pub track_name: String,
    pub artist_name: String,
    pub album_name: String,
    pub paused: bool,
    pub volume: i32,
    pub volume_max: i32,
    pub connected: bool,
    pub position: i64,
    pub duration: i64,
    pub last_pos_time: Instant,
    pub last_action: Instant,
    pub wheel_angle: f64,
    pub soundwave_bars: [f64; 24],
    pub soundwave_goals: [f64; 24],
    pub status_sync_boost_until: Instant,
    pub render_dirty: bool,

    // -- Mode & local playback --
    pub mode: AppMode,
    pub is_favorited: bool,
    pub local_was_playing: bool,

    // -- Playlist overlay --
    pub playlist_visible: bool,
    pub playlist_selected: usize,
    pub playlist_count: usize,
}

impl AppState {
    pub fn new() -> Self {
        let mut bars = [0.0f64; 24];
        let mut goals = [0.0f64; 24];
        crate::animation::reset_soundwave_idle(&mut bars, &mut goals);

        Self {
            current_track_uri: String::new(),
            track_name: String::new(),
            artist_name: String::new(),
            album_name: String::new(),
            paused: false,
            volume: 80,
            volume_max: 100,
            connected: false,
            position: 0,
            duration: 0,
            last_pos_time: Instant::now(),
            last_action: Instant::now(),
            wheel_angle: 0.0,
            soundwave_bars: bars,
            soundwave_goals: goals,
            status_sync_boost_until: Instant::now(),
            render_dirty: false,

            mode: AppMode::default(),
            is_favorited: false,
            local_was_playing: false,

            playlist_visible: false,
            playlist_selected: 0,
            playlist_count: 0,
        }
    }

    pub fn boost_status_sync(&mut self, now: Instant, duration: std::time::Duration) {
        self.status_sync_boost_until = now + duration;
    }

    pub fn set_paused(&mut self, paused: bool) {
        if self.paused != paused {
            self.paused = paused;
            self.render_dirty = true;
        }
    }

    pub fn set_connected(&mut self, connected: bool) {
        if self.connected != connected {
            self.connected = connected;
            self.render_dirty = true;
        }
    }

    pub fn set_volume(&mut self, volume: i32, volume_max: i32) {
        if self.volume != volume || self.volume_max != volume_max {
            self.volume = volume;
            self.volume_max = volume_max;
            self.render_dirty = true;
        }
    }

    pub fn set_position(&mut self, position: i64, now: Instant) {
        if self.position != position {
            self.position = position;
            self.render_dirty = true;
        }
        self.last_pos_time = now;
    }

    pub fn set_duration(&mut self, duration: i64) {
        if self.duration != duration {
            self.duration = duration;
            self.render_dirty = true;
        }
    }

    pub fn set_mode(&mut self, mode: AppMode) {
        if self.mode != mode {
            self.mode = mode;
            self.render_dirty = true;
        }
    }

    pub fn set_favorited(&mut self, favorited: bool) {
        if self.is_favorited != favorited {
            self.is_favorited = favorited;
            self.render_dirty = true;
        }
    }

    pub fn set_playlist_visible(&mut self, visible: bool) {
        if self.playlist_visible != visible {
            self.playlist_visible = visible;
            self.render_dirty = true;
        }
    }

    pub fn set_playlist_selected(&mut self, selected: usize) {
        if self.playlist_selected != selected {
            self.playlist_selected = selected;
            self.render_dirty = true;
        }
    }

    pub fn set_playlist_count(&mut self, count: usize) {
        if self.playlist_count != count {
            self.playlist_count = count;
            self.render_dirty = true;
        }
    }
}

/// Immutable assets loaded at startup.
pub struct Assets {
    pub tape_base: RgbaImage,
    pub tape_a: RgbaImage,
    pub taperoll: RgbaImage,
    pub wheel: RgbaImage,
    pub cover_mask: Option<RgbaImage>,
    pub playing: Option<RgbaImage>,
    pub paused: Option<RgbaImage>,
    pub spotify_on: Option<RgbaImage>,
    pub spotify_off: Option<RgbaImage>,
    pub fav_on: Option<RgbaImage>,
    pub fav_off: Option<RgbaImage>,
}

impl Assets {
    pub fn load() -> Self {
        use crate::resources::load_image_resource;

        Self {
            tape_base: load_image_resource("tapeBase.png")
                .expect("required resource: tapeBase.png"),
            tape_a: load_image_resource("tapeA.png").expect("required resource: tapeA.png"),
            taperoll: load_image_resource("taperoll.png")
                .expect("required resource: taperoll.png"),
            wheel: load_image_resource("wheel.png").expect("required resource: wheel.png"),
            cover_mask: load_image_resource("cover_mask.png"),
            playing: load_image_resource("play.png")
                .or_else(|| load_image_resource("playing.png")),
            paused: load_image_resource("pause.png")
                .or_else(|| load_image_resource("paused.png")),
            spotify_on: load_image_resource("spotify_on.png"),
            spotify_off: load_image_resource("spotify_off.png"),
            fav_on: load_image_resource("fav_on.png"),
            fav_off: load_image_resource("fav_off.png"),
        }
    }
}
