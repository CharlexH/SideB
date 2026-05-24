use std::time::Instant;

use crate::mode::AppMode;
use crate::types::RgbaImage;

const CONFIRM_WINDOW_SECS: u64 = 2;
const NOTICE_WINDOW_SECS: u64 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmationKind {
    ExitApp,
    RemoveFavorite { uri: String },
}

#[derive(Debug, Clone)]
pub struct ConfirmationState {
    pub kind: ConfirmationKind,
    pub until: Instant,
}

#[derive(Debug, Clone)]
pub struct NoticeState {
    pub message: &'static str,
    pub until: Instant,
}

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
    pub battery_percent: Option<u8>,
    pub battery_charging: bool,
    pub screen_locked: bool,

    // -- Mode & local playback --
    pub mode: AppMode,
    pub is_favorited: bool,
    pub spotify_preempted_local_uri: Option<String>,
    pub spotify_was_active: bool,
    pub stop_to_sleep_eligible: bool,

    // -- Playlist overlay --
    pub playlist_visible: bool,
    pub playlist_selected: usize,
    pub playlist_count: usize,

    // -- Confirmations --
    pub confirmation: Option<ConfirmationState>,
    pub notice: Option<NoticeState>,
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
            battery_percent: None,
            battery_charging: false,
            screen_locked: false,

            mode: AppMode::default(),
            is_favorited: false,
            spotify_preempted_local_uri: None,
            spotify_was_active: false,
            stop_to_sleep_eligible: false,

            playlist_visible: false,
            playlist_selected: 0,
            playlist_count: 0,

            confirmation: None,
            notice: None,
        }
    }

    pub fn request_exit_confirmation(&mut self, now: Instant) -> bool {
        if let Some(confirm) = &self.confirmation {
            if matches!(confirm.kind, ConfirmationKind::ExitApp) && now < confirm.until {
                return true;
            }
        }
        self.confirmation = Some(ConfirmationState {
            kind: ConfirmationKind::ExitApp,
            until: now + std::time::Duration::from_secs(CONFIRM_WINDOW_SECS),
        });
        self.render_dirty = true;
        false
    }

    pub fn request_remove_confirmation(&mut self, uri: &str, now: Instant) -> bool {
        if let Some(confirm) = &self.confirmation {
            if let ConfirmationKind::RemoveFavorite { uri: pending_uri } = &confirm.kind {
                if pending_uri == uri && now < confirm.until {
                    return true;
                }
            }
        }
        self.confirmation = Some(ConfirmationState {
            kind: ConfirmationKind::RemoveFavorite {
                uri: uri.to_string(),
            },
            until: now + std::time::Duration::from_secs(CONFIRM_WINDOW_SECS),
        });
        self.render_dirty = true;
        false
    }

    pub fn clear_confirmation(&mut self) {
        if self.confirmation.is_some() {
            self.confirmation = None;
            self.render_dirty = true;
        }
    }

    pub fn active_confirmation_message(&mut self, now: Instant) -> Option<&'static str> {
        match self.confirmation.as_ref() {
            Some(ConfirmationState {
                kind: ConfirmationKind::ExitApp,
                until,
            }) if now < *until => Some("Press B again to exit"),
            Some(ConfirmationState {
                kind: ConfirmationKind::RemoveFavorite { .. },
                until,
            }) if now < *until => Some("Press X again to remove favorite"),
            Some(_) => {
                self.confirmation = None;
                self.render_dirty = true;
                None
            }
            None => None,
        }
    }

    pub fn show_notice(&mut self, message: &'static str, now: Instant) {
        self.notice = Some(NoticeState {
            message,
            until: now + std::time::Duration::from_secs(NOTICE_WINDOW_SECS),
        });
        self.render_dirty = true;
    }

    pub fn active_notice_message(&mut self, now: Instant) -> Option<&'static str> {
        match self.notice.as_ref() {
            Some(NoticeState { message, until }) if now < *until => Some(message),
            Some(_) => {
                self.notice = None;
                self.render_dirty = true;
                None
            }
            None => None,
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

    pub fn set_battery_percent(&mut self, percent: Option<u8>) {
        self.set_battery_snapshot(percent, self.battery_charging);
    }

    pub fn set_battery_snapshot(&mut self, percent: Option<u8>, charging: bool) {
        let percent = percent.filter(|value| *value <= 100);
        if self.battery_percent != percent || self.battery_charging != charging {
            self.battery_percent = percent;
            self.battery_charging = charging;
            self.render_dirty = true;
        }
    }

    pub fn set_screen_locked(&mut self, locked: bool) {
        if self.screen_locked != locked {
            self.screen_locked = locked;
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

    pub fn set_stop_to_sleep_eligible(&mut self, eligible: bool) {
        self.stop_to_sleep_eligible = eligible;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_confirmation_requires_two_presses_for_same_uri() {
        let now = Instant::now();
        let mut state = AppState::new();

        assert!(!state.request_remove_confirmation("local:track-1", now));
        assert_eq!(
            state.active_confirmation_message(now),
            Some("Press X again to remove favorite")
        );
        assert!(state.request_remove_confirmation("local:track-1", now));
    }

    #[test]
    fn remove_confirmation_does_not_confirm_for_different_uri() {
        let now = Instant::now();
        let mut state = AppState::new();

        assert!(!state.request_remove_confirmation("spotify:track:a", now));
        assert!(!state.request_remove_confirmation("spotify:track:b", now));
    }

    #[test]
    fn expired_confirmation_clears_itself() {
        let mut state = AppState::new();
        let now = Instant::now();
        state.confirmation = Some(ConfirmationState {
            kind: ConfirmationKind::ExitApp,
            until: now,
        });

        assert_eq!(
            state.active_confirmation_message(now + std::time::Duration::from_millis(1)),
            None
        );
        assert!(state.confirmation.is_none());
    }

    #[test]
    fn transient_notice_lasts_three_seconds() {
        let mut state = AppState::new();
        let now = Instant::now();

        state.show_notice("Missing ffmpeg", now);

        assert_eq!(state.active_notice_message(now), Some("Missing ffmpeg"));
        assert_eq!(
            state.active_notice_message(now + std::time::Duration::from_millis(2999)),
            Some("Missing ffmpeg")
        );
        assert_eq!(
            state.active_notice_message(now + std::time::Duration::from_secs(3)),
            None
        );
    }

    #[test]
    fn battery_percent_changes_mark_render_dirty_once() {
        let mut state = AppState::new();
        assert_eq!(state.battery_percent, None);

        state.set_battery_percent(Some(87));
        assert_eq!(state.battery_percent, Some(87));
        assert!(state.render_dirty);

        state.render_dirty = false;
        state.set_battery_percent(Some(87));
        assert!(!state.render_dirty);

        state.set_battery_percent(None);
        assert_eq!(state.battery_percent, None);
        assert!(state.render_dirty);
    }

    #[test]
    fn battery_charging_changes_mark_render_dirty_once() {
        let mut state = AppState::new();
        assert_eq!(state.battery_percent, None);
        assert!(!state.battery_charging);

        state.set_battery_snapshot(Some(50), true);
        assert_eq!(state.battery_percent, Some(50));
        assert!(state.battery_charging);
        assert!(state.render_dirty);

        state.render_dirty = false;
        state.set_battery_snapshot(Some(50), true);
        assert!(!state.render_dirty);

        state.set_battery_snapshot(Some(50), false);
        assert_eq!(state.battery_percent, Some(50));
        assert!(!state.battery_charging);
        assert!(state.render_dirty);
    }

    #[test]
    fn screen_lock_changes_mark_render_dirty_once() {
        let mut state = AppState::new();
        assert!(!state.screen_locked);

        state.set_screen_locked(true);
        assert!(state.screen_locked);
        assert!(state.render_dirty);

        state.render_dirty = false;
        state.set_screen_locked(true);
        assert!(!state.render_dirty);

        state.set_screen_locked(false);
        assert!(!state.screen_locked);
        assert!(state.render_dirty);
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
    pub bat0: Option<RgbaImage>,
    pub bat25: Option<RgbaImage>,
    pub bat50: Option<RgbaImage>,
    pub bat75: Option<RgbaImage>,
    pub bat100: Option<RgbaImage>,
    pub bat_charging: Option<RgbaImage>,
}

impl Assets {
    pub fn load() -> Self {
        use crate::resources::load_image_resource;

        Self {
            tape_base: load_image_resource("tapeBase.png")
                .expect("required resource: tapeBase.png"),
            tape_a: load_image_resource("tapeA.png").expect("required resource: tapeA.png"),
            taperoll: load_image_resource("taperoll.png").expect("required resource: taperoll.png"),
            wheel: load_image_resource("wheel.png").expect("required resource: wheel.png"),
            cover_mask: load_image_resource("cover_mask.png"),
            playing: load_image_resource("play.png").or_else(|| load_image_resource("playing.png")),
            paused: load_image_resource("pause.png").or_else(|| load_image_resource("paused.png")),
            spotify_on: load_image_resource("spotify_on.png"),
            spotify_off: load_image_resource("spotify_off.png"),
            fav_on: load_image_resource("fav_on.png"),
            fav_off: load_image_resource("fav_off.png"),
            bat0: load_image_resource("bat0.png"),
            bat25: load_image_resource("bat25.png"),
            bat50: load_image_resource("bat50.png"),
            bat75: load_image_resource("bat75.png"),
            bat100: load_image_resource("bat100.png"),
            bat_charging: load_image_resource("bat_charging.png"),
        }
    }
}
