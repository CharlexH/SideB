use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::animation;
use crate::app::AppState;
use crate::constants::*;
use crate::drawing;
use crate::favorites::FavoritesManager;
use crate::font::FontSet;
use crate::framebuffer::Framebuffer;
use crate::image_ops;
use crate::mode::AppMode;
use crate::playlist_view;
use crate::types::RgbaImage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverUpdate {
    Noop,
    Clear,
    Fetch(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FramePlan {
    should_render: bool,
    sleep: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatusBarLayout {
    time_x: i32,
    play_icon_x: i32,
    center_left: i32,
    center_right: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatteryIconLevel {
    Empty,
    Quarter,
    Half,
    ThreeQuarter,
    Full,
    Charging,
}

pub(crate) const STATUS_BAR_DIRTY_RECT: (usize, usize, usize, usize) = (0, 620, SCREEN_W, 690);
const STATUS_BAR_RIGHT_MARGIN: i32 = 28;
const STATUS_BAR_GAP: i32 = 18;

#[derive(Debug)]
struct AnimationMode {
    target_fps: u64,
    fast_frame_streak: u32,
    samples: VecDeque<Duration>,
    last_log_at: Instant,
}

impl AnimationMode {
    const FAST_FRAME_PROMOTION_COUNT: u32 = 120;
    const LOG_INTERVAL: Duration = Duration::from_secs(5);

    fn new() -> Self {
        Self {
            target_fps: BASE_ANIM_FPS,
            fast_frame_streak: 0,
            samples: VecDeque::with_capacity(MAX_ANIM_FPS as usize * 5),
            last_log_at: Instant::now(),
        }
    }

    fn target_fps(&self) -> u64 {
        self.target_fps
    }

    fn reset(&mut self, now: Instant) {
        self.target_fps = BASE_ANIM_FPS;
        self.fast_frame_streak = 0;
        self.samples.clear();
        self.last_log_at = now;
    }

    fn record_render(&mut self, render_cost: Duration, now: Instant) {
        const SAMPLE_CAPACITY: usize = MAX_ANIM_FPS as usize * 5;
        let fast_frame_budget = Duration::from_nanos(1_000_000_000 / MAX_ANIM_FPS);
        let fast_frame_threshold = fast_frame_budget.mul_f64(0.75);
        let slow_frame_threshold = fast_frame_budget.mul_f64(0.95);

        if self.samples.len() == SAMPLE_CAPACITY {
            self.samples.pop_front();
        }
        self.samples.push_back(render_cost);

        if MAX_ANIM_FPS > BASE_ANIM_FPS {
            if self.target_fps == BASE_ANIM_FPS && render_cost <= fast_frame_threshold {
                self.fast_frame_streak += 1;
                if self.fast_frame_streak >= Self::FAST_FRAME_PROMOTION_COUNT {
                    self.target_fps = MAX_ANIM_FPS;
                    self.fast_frame_streak = 0;
                    eprintln!("animation mode: boosted to {MAX_ANIM_FPS} FPS");
                }
            } else if self.target_fps == BASE_ANIM_FPS {
                self.fast_frame_streak = 0;
            }

            if self.target_fps == MAX_ANIM_FPS && render_cost > slow_frame_threshold {
                self.target_fps = BASE_ANIM_FPS;
                self.fast_frame_streak = 0;
                eprintln!(
                    "animation mode: dropped to {BASE_ANIM_FPS} FPS after {} ms frame",
                    render_cost.as_millis()
                );
            }
        }

        if now.duration_since(self.last_log_at) >= Self::LOG_INTERVAL && !self.samples.is_empty() {
            let total = self
                .samples
                .iter()
                .fold(Duration::ZERO, |acc, sample| acc + *sample);
            let avg_ms = total.as_secs_f64() * 1000.0 / self.samples.len() as f64;

            let mut sorted = self.samples.iter().copied().collect::<Vec<_>>();
            sorted.sort_unstable();
            let p95_index = ((sorted.len().saturating_sub(1)) as f64 * 0.95).round() as usize;
            let p95_ms = sorted[p95_index].as_secs_f64() * 1000.0;

            eprintln!(
                "anim perf: target={}fps avg={avg_ms:.2}ms p95={p95_ms:.2}ms samples={}",
                self.target_fps,
                self.samples.len()
            );
            self.last_log_at = now;
        }
    }
}

fn frame_plan(
    connected: bool,
    paused: bool,
    dirty: bool,
    full_redraw: bool,
    anim_fps: u64,
) -> FramePlan {
    let active_frame = Duration::from_nanos(1_000_000_000 / anim_fps);
    if connected && !paused {
        return FramePlan {
            should_render: true,
            sleep: active_frame,
        };
    }

    // When paused/idle and nothing to redraw, sleep longer to save CPU
    let idle_frame = if dirty || full_redraw {
        Duration::from_millis(100)
    } else {
        Duration::from_millis(300)
    };

    FramePlan {
        should_render: dirty || full_redraw,
        sleep: idle_frame,
    }
}

fn sync_scene_mode(render_state: &mut RenderState, last_connected: bool, connected: bool) {
    if last_connected != connected {
        render_state.full_redraw = true;
    }
}

fn truncate_chars_with_ellipsis(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return text.to_string();
    }

    if max_chars == 1 {
        return "…".to_string();
    }

    chars
        .into_iter()
        .take(max_chars - 1)
        .chain(std::iter::once('…'))
        .collect()
}

fn format_track_info(track_name: &str, artist_name: &str, max_chars: usize) -> Option<String> {
    let track_name = track_name.trim();
    let artist_name = artist_name.trim();

    let text = if track_name.is_empty() && artist_name.is_empty() {
        return None;
    } else if artist_name.is_empty() {
        track_name.to_string()
    } else if track_name.is_empty() {
        artist_name.to_string()
    } else {
        format!("{track_name} - {artist_name}")
    };

    Some(truncate_chars_with_ellipsis(&text, max_chars))
}

fn centered_text_x(screen_width: i32, text_width: i32) -> i32 {
    ((screen_width - text_width).max(0)) / 2
}

fn centered_text_x_in_bounds(left: i32, right: i32, text_width: i32) -> i32 {
    let available = (right - left).max(0);
    left + ((available - text_width).max(0)) / 2
}

fn battery_icon_level(percent: Option<u8>, charging: bool) -> Option<BatteryIconLevel> {
    if charging {
        return Some(BatteryIconLevel::Charging);
    }

    match percent? {
        80..=100 => Some(BatteryIconLevel::Full),
        60..=79 => Some(BatteryIconLevel::ThreeQuarter),
        40..=59 => Some(BatteryIconLevel::Half),
        20..=39 => Some(BatteryIconLevel::Quarter),
        _ => Some(BatteryIconLevel::Empty),
    }
}

fn draw_battery_status_icon(buf: &mut [u8], rs: &RenderState, percent: Option<u8>, charging: bool) {
    let Some(level) = battery_icon_level(percent, charging) else {
        return;
    };
    if let Some(img) = rs.battery_icon(level) {
        drawing::draw_image_alpha(buf, img, BATTERY_ICON_X, BAR_ICON_Y);
    }
}

fn status_bar_layout(screen_width: i32, time_width: i32) -> StatusBarLayout {
    let right_edge = screen_width - STATUS_BAR_RIGHT_MARGIN;
    let time_x = right_edge - time_width;
    let play_icon_x = time_x - PLAY_ICON_MARGIN - BAR_ICON_SIZE;

    StatusBarLayout {
        time_x,
        play_icon_x,
        center_left: FAV_ICON_X + BAR_ICON_SIZE + STATUS_BAR_GAP,
        center_right: play_icon_x - STATUS_BAR_GAP,
    }
}

fn format_track_info_to_width<F>(
    track_name: &str,
    artist_name: &str,
    max_chars: usize,
    max_width: i32,
    measure: F,
) -> Option<String>
where
    F: Fn(&str) -> i32,
{
    if max_width <= 0 {
        return None;
    }

    for chars in (1..=max_chars).rev() {
        let text = format_track_info(track_name, artist_name, chars)?;
        if measure(&text) <= max_width {
            return Some(text);
        }
    }

    None
}

fn dirty_rect_i32(rect: (usize, usize, usize, usize)) -> (i32, i32, i32, i32) {
    (rect.0 as i32, rect.1 as i32, rect.2 as i32, rect.3 as i32)
}

const FOOTER_NOTICE_DIRTY_RECT: (usize, usize, usize, usize) =
    (0, (HINTS_BASELINE_Y - 28) as usize, SCREEN_W, SCREEN_H);

fn playback_footer_labels() -> [&'static str; 6] {
    [
        "PREV/NEXT (←/→)",
        "VOL+/- (↑/↓)",
        "PLAY/PAUSE (A)",
        "FAV (X)",
        "LIST (Y)",
        "EXIT (B)",
    ]
}

fn waiting_status_message(startup_loading: bool) -> &'static str {
    if startup_loading {
        "Loading Tape"
    } else {
        "Waiting for Spotify or MP3 import"
    }
}

fn waiting_exit_hint(startup_loading: bool) -> Option<&'static str> {
    if startup_loading {
        None
    } else {
        Some("EXIT [B]")
    }
}

fn draw_footer_hints(buf: &mut [u8], fonts: &FontSet) {
    let hint_labels = playback_footer_labels();

    let total_width: i32 = hint_labels
        .iter()
        .map(|l| fonts.measure_text(l, fonts.scale_small))
        .sum();

    let start_x = 28;
    let available = (SCREEN_W as i32 - 56) - total_width;
    let gap = if hint_labels.len() > 1 {
        if available > 0 {
            available / (hint_labels.len() as i32 - 1)
        } else {
            4
        }
    } else {
        0
    };

    let mut x = start_x;
    for label in &hint_labels {
        fonts.draw_text(
            buf,
            label,
            x,
            HINTS_BASELINE_Y,
            0x3D,
            0x3D,
            0x3D,
            fonts.scale_small,
        );
        x += fonts.measure_text(label, fonts.scale_small) + gap;
    }
}

fn draw_waiting_footer(buf: &mut [u8], fonts: &FontSet, message: &str, exit_hint: Option<&str>) {
    drawing::fill_rect(
        buf,
        0,
        STATUS_BASELINE_Y - 32,
        SCREEN_W as i32,
        SCREEN_H as i32 - (STATUS_BASELINE_Y - 32),
        0,
        0,
        0,
        255,
    );

    let msg_w = fonts.measure_text(message, fonts.scale_large);
    fonts.draw_text(
        buf,
        message,
        SCREEN_W as i32 / 2 - msg_w / 2,
        STATUS_BASELINE_Y,
        255,
        255,
        255,
        fonts.scale_large,
    );

    if let Some(exit_hint) = exit_hint {
        let hint_w = fonts.measure_text(exit_hint, fonts.scale_small);
        fonts.draw_text(
            buf,
            exit_hint,
            SCREEN_W as i32 / 2 - hint_w / 2,
            HINTS_BASELINE_Y,
            255,
            255,
            255,
            fonts.scale_small,
        );
    }
}

fn draw_waiting_text(buf: &mut [u8], fonts: &FontSet, startup_loading: bool) {
    draw_waiting_footer(
        buf,
        fonts,
        waiting_status_message(startup_loading),
        waiting_exit_hint(startup_loading),
    );
}

pub(crate) fn draw_waiting_import_progress(buf: &mut [u8], fonts: &FontSet, message: &str) {
    draw_waiting_footer(buf, fonts, message, None);
}

fn draw_footer_notice(buf: &mut [u8], fonts: &FontSet, message: &str) {
    drawing::fill_rect(
        buf,
        FOOTER_NOTICE_DIRTY_RECT.0 as i32,
        FOOTER_NOTICE_DIRTY_RECT.1 as i32,
        (FOOTER_NOTICE_DIRTY_RECT.2 - FOOTER_NOTICE_DIRTY_RECT.0) as i32,
        (FOOTER_NOTICE_DIRTY_RECT.3 - FOOTER_NOTICE_DIRTY_RECT.1) as i32,
        0,
        0,
        0,
        255,
    );

    let msg_w = fonts.measure_text(message, fonts.scale_small);
    let msg_x = centered_text_x(SCREEN_W as i32, msg_w);
    fonts.draw_text(
        buf,
        message,
        msg_x,
        HINTS_BASELINE_Y,
        255,
        255,
        255,
        fonts.scale_small,
    );
}

pub fn build_startup_scene(
    tape_base: &RgbaImage,
    tape_a: &RgbaImage,
    taperoll: &RgbaImage,
    wheel: &RgbaImage,
    fonts: &FontSet,
) -> Vec<u8> {
    let overlay_window = image_ops::build_overlay_window(tape_a);
    let foreground = image_ops::build_cassette_foreground(tape_base, &overlay_window);
    let mut scene = vec![0u8; FB_SIZE];

    drawing::clear_buffer(&mut scene, 0, 0, 0, 255);
    draw_footer_hints(&mut scene, fonts);

    let left_roll = image_ops::scale_nearest(taperoll, LEFT_ROLL_MIN_SIZE as u32);
    let right_roll = image_ops::scale_nearest(taperoll, RIGHT_ROLL_MAX_SIZE as u32);
    drawing::draw_image_alpha(
        &mut scene,
        &left_roll,
        LEFT_ROLL_CENTER_X - LEFT_ROLL_MIN_SIZE / 2,
        ROLL_CENTER_Y - LEFT_ROLL_MIN_SIZE / 2,
    );
    drawing::draw_image_alpha(
        &mut scene,
        &right_roll,
        RIGHT_ROLL_CENTER_X - RIGHT_ROLL_MAX_SIZE / 2,
        ROLL_CENTER_Y - RIGHT_ROLL_MAX_SIZE / 2,
    );

    drawing::draw_image_alpha(&mut scene, &foreground, TAPE_BASE_X, TAPE_BASE_Y);
    drawing::draw_image_alpha(&mut scene, wheel, LEFT_WHEEL_X, LEFT_WHEEL_Y);
    drawing::draw_image_alpha(&mut scene, wheel, RIGHT_WHEEL_X, RIGHT_WHEEL_Y);

    draw_waiting_text(&mut scene, fonts, true);
    scene
}

/// Holds all pre-computed scene buffers and caches.
pub struct RenderState {
    pub scene_base: Vec<u8>,
    pub scene_playing: Vec<u8>,
    pub scene_waiting: Vec<u8>,
    pub scene_foreground: Option<RgbaImage>,
    pub scene_cover: Option<RgbaImage>,
    pub wheel_frames: Vec<RgbaImage>,
    pub taperoll_cache: HashMap<i32, Vec<RgbaImage>>,
    pub full_redraw: bool,
    // Keep references to assets needed for scene rebuilds
    pub cover_mask: Option<RgbaImage>,
    pub img_playing: Option<RgbaImage>,
    pub img_paused: Option<RgbaImage>,
    pub img_spotify_on: Option<RgbaImage>,
    pub img_spotify_off: Option<RgbaImage>,
    pub img_fav_on: Option<RgbaImage>,
    pub img_fav_off: Option<RgbaImage>,
    pub img_bat0: Option<RgbaImage>,
    pub img_bat25: Option<RgbaImage>,
    pub img_bat50: Option<RgbaImage>,
    pub img_bat75: Option<RgbaImage>,
    pub img_bat100: Option<RgbaImage>,
    pub img_bat_charging: Option<RgbaImage>,
    pub requested_cover_url: Option<String>,
    pub applied_cover_url: Option<String>,
}

impl RenderState {
    /// Initialize all render caches from loaded assets.
    pub fn init(
        tape_base: &RgbaImage,
        tape_a: &RgbaImage,
        taperoll: &RgbaImage,
        wheel: &RgbaImage,
        cover_mask: Option<RgbaImage>,
        img_playing: Option<RgbaImage>,
        img_paused: Option<RgbaImage>,
        img_spotify_on: Option<RgbaImage>,
        img_spotify_off: Option<RgbaImage>,
        img_fav_on: Option<RgbaImage>,
        img_fav_off: Option<RgbaImage>,
        img_bat0: Option<RgbaImage>,
        img_bat25: Option<RgbaImage>,
        img_bat50: Option<RgbaImage>,
        img_bat75: Option<RgbaImage>,
        img_bat100: Option<RgbaImage>,
        img_bat_charging: Option<RgbaImage>,
        fonts: &FontSet,
    ) -> Self {
        let overlay_window = image_ops::build_overlay_window(tape_a);
        let scene_foreground = image_ops::build_cassette_foreground(tape_base, &overlay_window);
        let wheel_frames = image_ops::build_rotated_frames(wheel, ROTATION_FRAME_COUNT);
        let taperoll_cache = image_ops::build_taperoll_frame_cache(taperoll, TAPEROLL_FRAME_COUNT);

        let mut rs = Self {
            scene_base: vec![0u8; FB_SIZE],
            scene_playing: vec![0u8; FB_SIZE],
            scene_waiting: vec![0u8; FB_SIZE],
            scene_foreground: Some(scene_foreground),
            scene_cover: None,
            wheel_frames,
            taperoll_cache,
            full_redraw: true,
            cover_mask,
            img_playing,
            img_paused,
            img_spotify_on,
            img_spotify_off,
            img_fav_on,
            img_fav_off,
            img_bat0,
            img_bat25,
            img_bat50,
            img_bat75,
            img_bat100,
            img_bat_charging,
            requested_cover_url: None,
            applied_cover_url: None,
        };

        rs.rebuild_base_scene(fonts);
        rs.rebuild_playing_scene_locked(None);
        rs.rebuild_waiting_scene(fonts);
        rs
    }

    fn battery_icon(&self, level: BatteryIconLevel) -> Option<&RgbaImage> {
        match level {
            BatteryIconLevel::Empty => self.img_bat0.as_ref(),
            BatteryIconLevel::Quarter => self.img_bat25.as_ref(),
            BatteryIconLevel::Half => self.img_bat50.as_ref(),
            BatteryIconLevel::ThreeQuarter => self.img_bat75.as_ref(),
            BatteryIconLevel::Full => self.img_bat100.as_ref(),
            BatteryIconLevel::Charging => self.img_bat_charging.as_ref(),
        }
    }

    /// Draw the base scene (hint labels at bottom).
    fn rebuild_base_scene(&mut self, fonts: &FontSet) {
        drawing::clear_buffer(&mut self.scene_base, 0, 0, 0, 255);
        draw_footer_hints(&mut self.scene_base, fonts);
    }

    /// Rebuild the playing scene with optional cover art.
    pub fn rebuild_playing_scene(&mut self, cover: Option<&RgbaImage>) {
        self.rebuild_playing_scene_locked(cover);
    }

    fn rebuild_playing_scene_locked(&mut self, cover: Option<&RgbaImage>) {
        self.scene_playing.copy_from_slice(&self.scene_base);
        self.scene_cover =
            cover.map(|img| image_ops::build_masked_cover(img, self.cover_mask.as_ref()));
        if let Some(cover) = &self.scene_cover {
            drawing::draw_image_alpha(&mut self.scene_playing, cover, COVER_X, COVER_Y);
        }
        self.full_redraw = true;
    }

    pub fn plan_cover_update(&mut self, cover_url: Option<&str>) -> CoverUpdate {
        let Some(cover_url) = cover_url.filter(|url| !url.is_empty()) else {
            let had_cover = self.scene_cover.is_some()
                || self.requested_cover_url.is_some()
                || self.applied_cover_url.is_some();
            self.requested_cover_url = None;
            self.applied_cover_url = None;
            if had_cover {
                self.rebuild_playing_scene(None);
                return CoverUpdate::Clear;
            }
            return CoverUpdate::Noop;
        };

        if self.requested_cover_url.as_deref() == Some(cover_url)
            || self.applied_cover_url.as_deref() == Some(cover_url)
        {
            return CoverUpdate::Noop;
        }

        let had_visible_cover = self.scene_cover.is_some() || self.applied_cover_url.is_some();
        self.requested_cover_url = Some(cover_url.to_string());
        self.applied_cover_url = None;
        if had_visible_cover {
            self.rebuild_playing_scene(None);
        }
        CoverUpdate::Fetch(cover_url.to_string())
    }

    pub fn apply_cover_if_current(&mut self, cover_url: &str, cover: &RgbaImage) -> bool {
        if self.requested_cover_url.as_deref() != Some(cover_url) {
            return false;
        }

        self.applied_cover_url = Some(cover_url.to_string());
        self.rebuild_playing_scene(Some(cover));
        true
    }

    pub fn replace_cover(&mut self, cover_url: &str, cover: &RgbaImage) {
        self.requested_cover_url = Some(cover_url.to_string());
        self.applied_cover_url = Some(cover_url.to_string());
        self.rebuild_playing_scene(Some(cover));
    }

    /// Rebuild the waiting scene (static cassette + "Waiting..." text).
    fn rebuild_waiting_scene(&mut self, fonts: &FontSet) {
        self.scene_waiting.copy_from_slice(&self.scene_base);

        // Draw static taperolls at initial positions
        if let Some(frames) = self.taperoll_cache.get(&LEFT_ROLL_MIN_SIZE) {
            if let Some(frame) = frames.first() {
                drawing::draw_image_alpha(
                    &mut self.scene_waiting,
                    frame,
                    LEFT_ROLL_CENTER_X - LEFT_ROLL_MIN_SIZE / 2,
                    ROLL_CENTER_Y - LEFT_ROLL_MIN_SIZE / 2,
                );
            }
        }
        if let Some(frames) = self.taperoll_cache.get(&RIGHT_ROLL_MAX_SIZE) {
            if let Some(frame) = frames.first() {
                drawing::draw_image_alpha(
                    &mut self.scene_waiting,
                    frame,
                    RIGHT_ROLL_CENTER_X - RIGHT_ROLL_MAX_SIZE / 2,
                    ROLL_CENTER_Y - RIGHT_ROLL_MAX_SIZE / 2,
                );
            }
        }

        // Draw cassette foreground
        if let Some(fg) = &self.scene_foreground {
            drawing::draw_image_alpha(&mut self.scene_waiting, fg, TAPE_BASE_X, TAPE_BASE_Y);
        }

        // Draw static wheels
        if let Some(wf) = self.wheel_frames.first() {
            drawing::draw_image_alpha(&mut self.scene_waiting, wf, LEFT_WHEEL_X, LEFT_WHEEL_Y);
            drawing::draw_image_alpha(&mut self.scene_waiting, wf, RIGHT_WHEEL_X, RIGHT_WHEEL_Y);
        }

        draw_waiting_text(&mut self.scene_waiting, fonts, false);

        self.full_redraw = true;
    }

    /// Get taperoll frames for a given (quantized) size.
    fn taperoll_frames_for_size(&self, size: i32) -> Option<&Vec<RgbaImage>> {
        let quantized = image_ops::quantize_roll_size(size);
        self.taperoll_cache.get(&quantized)
    }
}

/// Main render function — draws current state to back_buf.
pub fn render(
    back_buf: &mut [u8],
    app_state: &Arc<Mutex<AppState>>,
    render_state: &mut RenderState,
) {
    // Snapshot state
    let (_paused, mode, position, duration, wheel_angle, _soundwave_bars) = {
        let st = app_state.lock().unwrap();
        (
            st.paused,
            st.mode,
            st.position,
            st.duration,
            st.wheel_angle,
            st.soundwave_bars,
        )
    };

    if mode == crate::mode::AppMode::Waiting {
        back_buf.copy_from_slice(&render_state.scene_waiting);
        return;
    }

    // Dirty rects
    let dirty_rects: [(i32, i32, i32, i32); 4] = [
        (88, 64, 536, 520),                    // left roll
        (488, 64, 936, 520),                   // right roll
        dirty_rect_i32(STATUS_BAR_DIRTY_RECT), // info bar
        dirty_rect_i32(FOOTER_NOTICE_DIRTY_RECT),
    ];

    if render_state.full_redraw {
        back_buf.copy_from_slice(&render_state.scene_playing);
    } else {
        for &(x1, y1, x2, y2) in &dirty_rects {
            drawing::copy_rect(back_buf, &render_state.scene_playing, x1, y1, x2, y2);
        }
    }

    // Calculate progress and frame indices
    let progress = if duration > 0 {
        position as f64 / duration as f64
    } else {
        0.0
    };
    let (left_size, right_size) = image_ops::roll_sizes_for_progress(progress);
    let wheel_idx = image_ops::frame_index_for_angle(wheel_angle, render_state.wheel_frames.len());
    let roll_idx = image_ops::frame_index_for_angle(wheel_angle, TAPEROLL_FRAME_COUNT);

    let left_draw_size = image_ops::quantize_roll_size(left_size);
    let right_draw_size = image_ops::quantize_roll_size(right_size);

    // Draw taperolls
    if let Some(frames) = render_state.taperoll_frames_for_size(left_size) {
        if !frames.is_empty() {
            let idx = roll_idx % frames.len();
            drawing::draw_image_alpha(
                back_buf,
                &frames[idx],
                LEFT_ROLL_CENTER_X - left_draw_size / 2,
                ROLL_CENTER_Y - left_draw_size / 2,
            );
        }
    }
    if let Some(frames) = render_state.taperoll_frames_for_size(right_size) {
        if !frames.is_empty() {
            let idx = roll_idx % frames.len();
            drawing::draw_image_alpha(
                back_buf,
                &frames[idx],
                RIGHT_ROLL_CENTER_X - right_draw_size / 2,
                ROLL_CENTER_Y - right_draw_size / 2,
            );
        }
    }

    // Draw wheels
    if !render_state.wheel_frames.is_empty() {
        let wf = &render_state.wheel_frames[wheel_idx];
        drawing::draw_image_alpha(back_buf, wf, LEFT_WHEEL_X, LEFT_WHEEL_Y);
        drawing::draw_image_alpha(back_buf, wf, RIGHT_WHEEL_X, RIGHT_WHEEL_Y);
    }

    // Draw cassette foreground above the moving wheels/taperolls.
    if let Some(fg) = &render_state.scene_foreground {
        drawing::draw_image_alpha(back_buf, fg, TAPE_BASE_X, TAPE_BASE_Y);
    }

    // Cover should remain above the cassette foreground.
    if let Some(cover) = &render_state.scene_cover {
        drawing::draw_image_alpha(back_buf, cover, COVER_X, COVER_Y);
    }

    // Status icons are now drawn in the text overlay section of render_loop

    render_state.full_redraw = false;
}

/// 30 FPS render loop — updates animation state and calls render.
pub fn render_loop(
    fb: &Framebuffer,
    back_buf: &mut [u8],
    app_state: Arc<Mutex<AppState>>,
    render_state: Arc<Mutex<RenderState>>,
    fonts: &FontSet,
    quit: Arc<AtomicBool>,
    favorites: Arc<Mutex<FavoritesManager>>,
    download_progress: crate::download::DownloadProgressMap,
) {
    let mut last_frame = Instant::now();
    let mut last_connected = false;
    let mut last_playlist_visible = false;
    let mut animation_mode = AnimationMode::new();
    let mut last_wheel_frame: usize = usize::MAX; // track last rendered wheel frame
    let mut last_position: i64 = -1; // track last rendered position for time display
    let mut last_screen_locked = false;

    loop {
        if quit.load(Ordering::Relaxed) {
            return;
        }

        let now = Instant::now();
        let dt = now.duration_since(last_frame);
        last_frame = now;

        let (mode, paused, dirty, playlist_visible, screen_locked, importing);
        {
            let mut st = app_state.lock().unwrap();
            mode = st.mode;
            paused = st.paused;
            importing = st.import_progress.is_some();
            dirty = st.render_dirty
                || st.confirmation.is_some()
                || st.notice.is_some()
                || st.import_progress.is_some();
            playlist_visible = st.playlist_visible;
            screen_locked = st.screen_locked;

            // Animate wheels when playing (Spotify or Local)
            let playing = match mode {
                AppMode::Spotify => !st.paused,
                AppMode::Local => !st.paused,
                AppMode::Waiting => false,
            };

            if playing {
                st.wheel_angle = (st.wheel_angle
                    + 2.0 * std::f64::consts::PI * dt.as_secs_f64()
                        / WHEEL_ROTATION_PERIOD.as_secs_f64())
                    % (2.0 * std::f64::consts::PI);
            }

            // Advance position for Spotify mode (local position is tracked by LocalPlayer)
            if mode == AppMode::Spotify && !st.paused && st.duration > 0 {
                st.position += dt.as_millis() as i64;
                if st.position > st.duration {
                    st.position = st.duration;
                }
                st.last_pos_time = now;
            }
        }

        if screen_locked {
            if dirty || !last_screen_locked {
                back_buf.fill(0);
                fb.swap_buffers(back_buf);
                app_state.lock().unwrap().render_dirty = false;
            }
            last_screen_locked = true;
            animation_mode.reset(now);
            std::thread::sleep(std::time::Duration::from_millis(100));
            continue;
        } else if last_screen_locked {
            render_state.lock().unwrap().full_redraw = true;
            last_screen_locked = false;
        }

        let display_waiting_scene = importing || mode == AppMode::Waiting;
        // For scene mode sync, treat importing as waiting so the cassette waiting scene is shown.
        let scene_connected = !display_waiting_scene;
        let full_redraw = {
            let mut rs = render_state.lock().unwrap();
            sync_scene_mode(&mut rs, last_connected, scene_connected);
            // Force full redraw when playlist overlay is dismissed
            if last_playlist_visible && !playlist_visible {
                rs.full_redraw = true;
            }
            rs.full_redraw
        };
        last_connected = scene_connected;
        last_playlist_visible = playlist_visible;

        let scene_playing = !display_waiting_scene;
        let plan = frame_plan(
            scene_connected,
            paused,
            dirty,
            full_redraw,
            animation_mode.target_fps(),
        );

        if !plan.should_render {
            if !scene_playing || paused {
                animation_mode.reset(now);
            }
            std::thread::sleep(plan.sleep);
            continue;
        }

        let render_started = Instant::now();
        if display_waiting_scene {
            let mut rs = render_state.lock().unwrap();
            if rs.full_redraw || dirty {
                back_buf.copy_from_slice(&rs.scene_waiting);

                if importing {
                    if let Some(msg) = app_state.lock().unwrap().import_progress_message() {
                        draw_waiting_footer(back_buf, fonts, &msg, None);
                    }
                } else {
                    // Exit confirmation overlay on waiting screen
                    let mut st = app_state.lock().unwrap();
                    if let Some(msg) = st.active_confirmation_message(std::time::Instant::now()) {
                        let status_rect = STATUS_BAR_DIRTY_RECT;
                        drawing::fill_rect(
                            back_buf,
                            status_rect.0 as i32,
                            status_rect.1 as i32,
                            (status_rect.2 - status_rect.0) as i32,
                            (status_rect.3 - status_rect.1) as i32,
                            0,
                            0,
                            0,
                            255,
                        );
                        let msg_w = fonts.measure_text(msg, fonts.scale_large);
                        let msg_x = centered_text_x(SCREEN_W as i32, msg_w);
                        fonts.draw_text(
                            back_buf,
                            msg,
                            msg_x,
                            STATUS_BASELINE_Y,
                            200,
                            200,
                            200,
                            fonts.scale_large,
                        );
                    } else {
                        draw_battery_status_icon(
                            back_buf,
                            &rs,
                            st.battery_percent,
                            st.battery_charging,
                        );
                    }

                    if let Some(msg) = st.active_notice_message(std::time::Instant::now()) {
                        draw_footer_notice(back_buf, fonts, &msg);
                    }

                    // Playlist overlay on waiting screen
                    if playlist_visible {
                        render_playlist(
                            back_buf,
                            &app_state,
                            &favorites,
                            fonts,
                            &download_progress,
                        );
                    }
                }

                fb.swap_buffers(back_buf);
                rs.full_redraw = false;
            }
            drop(rs);
            app_state.lock().unwrap().render_dirty = false;
        } else {
            // Spotify or Local mode — show cassette playing scene
            let mut rs = render_state.lock().unwrap();
            let full_redraw = rs.full_redraw;
            render(back_buf, &app_state, &mut rs);
            drop(rs);

            // Draw bottom bar overlay (icons + track info + time)
            {
                let mut st = app_state.lock().unwrap();
                let rs = render_state.lock().unwrap();
                let now = std::time::Instant::now();
                let confirmation_msg = st.active_confirmation_message(now);
                let notice_msg = st.active_notice_message(now);

                if let Some(msg) = confirmation_msg {
                    let msg_w = fonts.measure_text(msg, fonts.scale_large);
                    let msg_x = centered_text_x(SCREEN_W as i32, msg_w);
                    fonts.draw_text(
                        back_buf,
                        msg,
                        msg_x,
                        STATUS_BASELINE_Y,
                        200,
                        200,
                        200,
                        fonts.scale_large,
                    );
                } else {
                    // Left side: battery, Spotify connection, favorite.
                    draw_battery_status_icon(
                        back_buf,
                        &rs,
                        st.battery_percent,
                        st.battery_charging,
                    );

                    let spotify_icon = if st.connected {
                        &rs.img_spotify_on
                    } else {
                        &rs.img_spotify_off
                    };
                    if let Some(img) = spotify_icon {
                        drawing::draw_image_alpha(back_buf, img, SPOTIFY_ICON_X, BAR_ICON_Y);
                    }

                    if !st.current_track_uri.is_empty() {
                        let fav_icon = if st.is_favorited {
                            &rs.img_fav_on
                        } else {
                            &rs.img_fav_off
                        };
                        if let Some(img) = fav_icon {
                            drawing::draw_image_alpha(back_buf, img, FAV_ICON_X, BAR_ICON_Y);
                        }
                    }

                    // Right side: time remaining
                    let time_remaining = animation::format_duration(st.duration - st.position);
                    let tr_w = fonts.measure_text(&time_remaining, fonts.scale_large);
                    let layout = status_bar_layout(SCREEN_W as i32, tr_w);
                    fonts.draw_text(
                        back_buf,
                        &time_remaining,
                        layout.time_x,
                        STATUS_BASELINE_Y,
                        255,
                        255,
                        255,
                        fonts.scale_large,
                    );

                    // Right side: play/pause icon (left of time)
                    let play_icon = if st.paused {
                        &rs.img_paused
                    } else {
                        &rs.img_playing
                    };
                    if let Some(img) = play_icon {
                        drawing::draw_image_alpha(back_buf, img, layout.play_icon_x, BAR_ICON_Y);
                    }

                    // Center: track info
                    let max_info_width = layout.center_right - layout.center_left;
                    if let Some(info_text) = format_track_info_to_width(
                        &st.track_name,
                        &st.artist_name,
                        30,
                        max_info_width,
                        |text| fonts.measure_text(text, fonts.scale_large),
                    ) {
                        let info_w = fonts.measure_text(&info_text, fonts.scale_large);
                        let info_x = centered_text_x_in_bounds(
                            layout.center_left,
                            layout.center_right,
                            info_w,
                        );
                        fonts.draw_text(
                            back_buf,
                            &info_text,
                            info_x,
                            STATUS_BASELINE_Y,
                            255,
                            255,
                            255,
                            fonts.scale_large,
                        );
                    }
                }

                if let Some(msg) = notice_msg {
                    draw_footer_notice(back_buf, fonts, &msg);
                }
            }

            // Playlist overlay on playing screen
            if playlist_visible {
                render_playlist(back_buf, &app_state, &favorites, fonts, &download_progress);
            }

            if full_redraw || playlist_visible {
                fb.swap_buffers(back_buf);
                last_wheel_frame = usize::MAX; // force next partial update
            } else {
                // Compute current wheel frame index to detect actual visual changes
                let (cur_wheel_frame, cur_position) = {
                    let st = app_state.lock().unwrap();
                    let wf_count = render_state.lock().unwrap().wheel_frames.len().max(1);
                    let idx = crate::image_ops::frame_index_for_angle(st.wheel_angle, wf_count);
                    (idx, st.position)
                };
                // Only write dirty rects if animation or time display actually changed
                let position_sec = cur_position / 1000;
                let last_position_sec = last_position / 1000;
                if cur_wheel_frame != last_wheel_frame || position_sec != last_position_sec || dirty
                {
                    let dirty_rects: [(usize, usize, usize, usize); 4] = [
                        (88, 64, 536, 520),
                        (488, 64, 936, 520),
                        STATUS_BAR_DIRTY_RECT,
                        FOOTER_NOTICE_DIRTY_RECT,
                    ];
                    for (x1, y1, x2, y2) in dirty_rects {
                        fb.copy_rect(back_buf, x1, y1, x2, y2);
                    }
                    last_wheel_frame = cur_wheel_frame;
                    last_position = cur_position;
                }
            }

            app_state.lock().unwrap().render_dirty = false;
        }

        if scene_playing && !paused {
            animation_mode.record_render(render_started.elapsed(), Instant::now());
        } else {
            animation_mode.reset(now);
        }

        // Sleep until next frame
        let elapsed = last_frame.elapsed();
        if elapsed < plan.sleep {
            std::thread::sleep(plan.sleep - elapsed);
        }
    }
}

/// Render the playlist overlay onto the back buffer.
fn render_playlist(
    buf: &mut [u8],
    app_state: &Arc<Mutex<AppState>>,
    favorites: &Arc<Mutex<FavoritesManager>>,
    fonts: &FontSet,
    download_progress: &crate::download::DownloadProgressMap,
) {
    let mut st = app_state.lock().unwrap();
    let selected = st.playlist_selected;
    let playing_uri = st.current_track_uri.clone();
    let confirm_message = st.active_confirmation_message(Instant::now());
    drop(st);

    let fav = favorites.lock().unwrap();
    let entries = fav.all_entries().to_vec();
    drop(fav);

    let uri_ref = if playing_uri.is_empty() {
        None
    } else {
        Some(playing_uri.as_str())
    };
    let progress_snapshot = download_progress.lock().unwrap().clone();
    playlist_view::render_playlist_overlay(
        buf,
        &entries,
        selected,
        uri_ref,
        confirm_message,
        fonts,
        &progress_snapshot,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_render_state() -> RenderState {
        RenderState {
            scene_base: vec![0u8; FB_SIZE],
            scene_playing: vec![0u8; FB_SIZE],
            scene_waiting: vec![0u8; FB_SIZE],
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
        }
    }

    #[test]
    fn frame_plan_skips_paused_frames_without_dirty_state() {
        let plan = frame_plan(true, true, false, false, BASE_ANIM_FPS);
        assert!(!plan.should_render);
        assert_eq!(plan.sleep, Duration::from_millis(300));
    }

    #[test]
    fn frame_plan_keeps_animating_while_playing() {
        let plan = frame_plan(true, false, false, false, BASE_ANIM_FPS);
        assert!(plan.should_render);
        assert_eq!(
            plan.sleep,
            Duration::from_nanos(1_000_000_000 / BASE_ANIM_FPS)
        );
    }

    #[test]
    fn dirty_paused_playback_frame_still_renders() {
        let plan = frame_plan(true, true, true, false, BASE_ANIM_FPS);
        assert!(plan.should_render);
        assert_eq!(plan.sleep, Duration::from_millis(100));
    }

    #[test]
    fn track_info_is_combined_without_truncation_when_short_enough() {
        assert_eq!(
            format_track_info("239", "perquel", 30).as_deref(),
            Some("239 - perquel")
        );
    }

    #[test]
    fn centered_text_x_uses_full_screen_width() {
        assert_eq!(centered_text_x(1024, 200), 412);
        assert_eq!(centered_text_x(1024, 1024), 0);
    }

    #[test]
    fn battery_icon_level_uses_charging_and_five_equal_percent_bands() {
        assert_eq!(
            battery_icon_level(Some(50), true),
            Some(BatteryIconLevel::Charging)
        );
        assert_eq!(
            battery_icon_level(None, true),
            Some(BatteryIconLevel::Charging)
        );
        assert_eq!(battery_icon_level(None, false), None);
        assert_eq!(
            battery_icon_level(Some(100), false),
            Some(BatteryIconLevel::Full)
        );
        assert_eq!(
            battery_icon_level(Some(80), false),
            Some(BatteryIconLevel::Full)
        );
        assert_eq!(
            battery_icon_level(Some(79), false),
            Some(BatteryIconLevel::ThreeQuarter)
        );
        assert_eq!(
            battery_icon_level(Some(60), false),
            Some(BatteryIconLevel::ThreeQuarter)
        );
        assert_eq!(
            battery_icon_level(Some(59), false),
            Some(BatteryIconLevel::Half)
        );
        assert_eq!(
            battery_icon_level(Some(40), false),
            Some(BatteryIconLevel::Half)
        );
        assert_eq!(
            battery_icon_level(Some(39), false),
            Some(BatteryIconLevel::Quarter)
        );
        assert_eq!(
            battery_icon_level(Some(20), false),
            Some(BatteryIconLevel::Quarter)
        );
        assert_eq!(
            battery_icon_level(Some(19), false),
            Some(BatteryIconLevel::Empty)
        );
        assert_eq!(
            battery_icon_level(Some(0), false),
            Some(BatteryIconLevel::Empty)
        );
    }

    #[test]
    fn status_bar_layout_orders_left_icons_battery_spotify_favorite() {
        let time_width = 74;
        let layout = status_bar_layout(SCREEN_W as i32, time_width);

        assert_eq!(BATTERY_ICON_X, 20);
        assert_eq!(SPOTIFY_ICON_X, BATTERY_ICON_X + BAR_ICON_SIZE + 16);
        assert_eq!(FAV_ICON_X, SPOTIFY_ICON_X + BAR_ICON_SIZE + 16);
        assert_eq!(
            layout.center_left,
            FAV_ICON_X + BAR_ICON_SIZE + STATUS_BAR_GAP
        );
        assert!(layout.play_icon_x + BAR_ICON_SIZE + PLAY_ICON_MARGIN <= layout.time_x);
        assert_eq!(
            layout.time_x + time_width,
            SCREEN_W as i32 - STATUS_BAR_RIGHT_MARGIN
        );
        assert!(layout.center_right <= layout.play_icon_x - STATUS_BAR_GAP);
        assert!(STATUS_BASELINE_Y as usize >= STATUS_BAR_DIRTY_RECT.1);
        assert!(STATUS_BASELINE_Y as usize <= STATUS_BAR_DIRTY_RECT.3);
    }

    #[test]
    fn bundled_battery_pngs_are_loaded_as_status_icons() {
        for name in [
            "bat0.png",
            "bat25.png",
            "bat50.png",
            "bat75.png",
            "bat100.png",
            "bat_charging.png",
        ] {
            let img = crate::resources::load_image_resource(name).expect("battery icon loads");
            assert_eq!(img.width, BAR_ICON_SIZE as u32, "{name} width");
            assert_eq!(img.height, BAR_ICON_SIZE as u32, "{name} height");
        }
    }

    #[test]
    fn track_info_is_truncated_to_thirty_characters_with_ellipsis() {
        assert_eq!(
            format_track_info("落日", "椎名林檎", 30).as_deref(),
            Some("落日 - 椎名林檎")
        );
        assert_eq!(
            format_track_info(
                "这是非常长非常长非常长的歌曲名字版本二",
                "这是很长很长的歌手名字版本二",
                30
            )
            .as_deref(),
            Some("这是非常长非常长非常长的歌曲名字版本二 - 这是很长很长的…")
        );
    }

    #[test]
    fn animation_mode_stays_at_thirty_fps_after_sustained_fast_frames() {
        let mut mode = AnimationMode::new();
        let now = Instant::now();

        for i in 0..120 {
            mode.record_render(
                Duration::from_millis(6),
                now + Duration::from_millis(i * 33),
            );
        }

        assert_eq!(mode.target_fps(), BASE_ANIM_FPS);
    }

    #[test]
    fn playback_footer_labels_match_reference_copy() {
        assert_eq!(
            playback_footer_labels(),
            [
                "PREV/NEXT (←/→)",
                "VOL+/- (↑/↓)",
                "PLAY/PAUSE (A)",
                "FAV (X)",
                "LIST (Y)",
                "EXIT (B)",
            ]
        );
    }

    #[test]
    fn waiting_status_message_switches_for_startup_loading() {
        assert_eq!(waiting_status_message(true), "Loading Tape");
        assert_eq!(
            waiting_status_message(false),
            "Waiting for Spotify or MP3 import"
        );
    }

    #[test]
    fn waiting_exit_hint_is_hidden_during_startup_loading() {
        assert_eq!(waiting_exit_hint(true), None);
        assert_eq!(waiting_exit_hint(false), Some("EXIT [B]"));
    }

    #[test]
    fn animation_mode_stays_at_thirty_fps_after_slow_frames() {
        let mut mode = AnimationMode::new();
        let now = Instant::now();

        for i in 0..120 {
            mode.record_render(
                Duration::from_millis(6),
                now + Duration::from_millis(i * 33),
            );
        }
        assert_eq!(mode.target_fps(), BASE_ANIM_FPS);

        mode.record_render(Duration::from_millis(20), now + Duration::from_secs(5));

        assert_eq!(mode.target_fps(), BASE_ANIM_FPS);
    }

    #[test]
    fn cover_update_fetches_a_new_url_only_once() {
        let mut rs = empty_render_state();

        assert_eq!(
            rs.plan_cover_update(Some("https://img/cover-a")),
            CoverUpdate::Fetch("https://img/cover-a".to_string())
        );
        assert_eq!(
            rs.plan_cover_update(Some("https://img/cover-a")),
            CoverUpdate::Noop
        );
    }

    #[test]
    fn cover_update_discards_stale_fetch_results() {
        let mut rs = empty_render_state();
        let img = RgbaImage::new(4, 4);

        assert_eq!(
            rs.plan_cover_update(Some("https://img/cover-a")),
            CoverUpdate::Fetch("https://img/cover-a".to_string())
        );
        assert_eq!(
            rs.plan_cover_update(Some("https://img/cover-b")),
            CoverUpdate::Fetch("https://img/cover-b".to_string())
        );

        assert!(!rs.apply_cover_if_current("https://img/cover-a", &img));
        assert!(rs.apply_cover_if_current("https://img/cover-b", &img));
    }

    #[test]
    fn cover_update_clears_previous_cover_while_new_one_is_pending() {
        let mut rs = empty_render_state();
        let img = RgbaImage::new(4, 4);

        assert_eq!(
            rs.plan_cover_update(Some("https://img/cover-a")),
            CoverUpdate::Fetch("https://img/cover-a".to_string())
        );
        assert!(rs.apply_cover_if_current("https://img/cover-a", &img));
        rs.full_redraw = false;

        assert_eq!(
            rs.plan_cover_update(Some("https://img/cover-b")),
            CoverUpdate::Fetch("https://img/cover-b".to_string())
        );

        assert!(rs.scene_cover.is_none());
        assert_eq!(
            rs.requested_cover_url.as_deref(),
            Some("https://img/cover-b")
        );
        assert_eq!(rs.applied_cover_url, None);
        assert!(rs.full_redraw);
    }

    #[test]
    fn rebuild_playing_scene_bakes_static_foreground_and_cover() {
        let mut rs = empty_render_state();

        let mut foreground = RgbaImage::new(1, 1);
        foreground.set_pixel(0, 0, 10, 20, 30, 255);
        rs.scene_foreground = Some(foreground);

        let mut cover = RgbaImage::new(1, 1);
        cover.set_pixel(0, 0, 200, 100, 50, 255);

        rs.rebuild_playing_scene(Some(&cover));

        let fg_offset = ((TAPE_BASE_Y as usize) * SCREEN_W + TAPE_BASE_X as usize) * BPP;
        assert_eq!(&rs.scene_playing[fg_offset..fg_offset + 4], &[0, 0, 0, 0]);

        let cover_offset = ((COVER_Y as usize) * SCREEN_W + COVER_X as usize) * BPP;
        assert_eq!(
            &rs.scene_playing[cover_offset..cover_offset + 4],
            &[50, 100, 200, 255]
        );
    }

    #[test]
    fn render_keeps_cover_visible_above_foreground() {
        let state = Arc::new(Mutex::new(AppState::new()));
        {
            let mut st = state.lock().unwrap();
            st.connected = true;
            st.mode = AppMode::Spotify;
        }

        let mut rs = empty_render_state();
        let mut foreground = RgbaImage::new(
            (COVER_X - TAPE_BASE_X + 1) as u32,
            (COVER_Y - TAPE_BASE_Y + 1) as u32,
        );
        foreground.set_pixel(
            (COVER_X - TAPE_BASE_X) as u32,
            (COVER_Y - TAPE_BASE_Y) as u32,
            10,
            20,
            30,
            255,
        );
        rs.scene_foreground = Some(foreground);

        let mut cover = RgbaImage::new(1, 1);
        cover.set_pixel(0, 0, 200, 100, 50, 255);
        rs.rebuild_playing_scene(Some(&cover));
        rs.full_redraw = true;

        let mut back_buf = vec![0u8; FB_SIZE];
        render(&mut back_buf, &state, &mut rs);

        let cover_offset = ((COVER_Y as usize) * SCREEN_W + COVER_X as usize) * BPP;
        assert_eq!(
            &back_buf[cover_offset..cover_offset + 4],
            &[50, 100, 200, 255]
        );
    }

    #[test]
    fn replace_cover_swaps_visible_cover_in_one_step() {
        let mut rs = empty_render_state();

        let mut old_cover = RgbaImage::new(1, 1);
        old_cover.set_pixel(0, 0, 200, 100, 50, 255);
        rs.replace_cover("https://img/cover-a", &old_cover);

        let mut new_cover = RgbaImage::new(1, 1);
        new_cover.set_pixel(0, 0, 20, 220, 40, 255);
        rs.full_redraw = false;
        rs.replace_cover("https://img/cover-b", &new_cover);

        assert_eq!(
            rs.requested_cover_url.as_deref(),
            Some("https://img/cover-b")
        );
        assert_eq!(rs.applied_cover_url.as_deref(), Some("https://img/cover-b"));
        assert!(rs.scene_cover.is_some());

        let cover = rs.scene_cover.as_ref().unwrap();
        assert_eq!(cover.pixel_at(0, 0), (20, 220, 40, 255));
        assert!(rs.full_redraw);
    }

    #[test]
    fn scene_mode_switch_forces_full_redraw() {
        let mut rs = empty_render_state();
        rs.full_redraw = false;

        sync_scene_mode(&mut rs, false, true);

        assert!(rs.full_redraw);
    }

    #[test]
    fn scene_mode_stable_does_not_force_full_redraw() {
        let mut rs = empty_render_state();
        rs.full_redraw = false;

        sync_scene_mode(&mut rs, true, true);

        assert!(!rs.full_redraw);
    }
}
