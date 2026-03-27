use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::animation;
use crate::app::AppState;
use crate::constants::*;
use crate::drawing;
use crate::font::FontSet;
use crate::framebuffer::Framebuffer;
use crate::image_ops;
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
    let idle_frame = Duration::from_millis(100);

    if connected && !paused {
        return FramePlan {
            should_render: true,
            sleep: active_frame,
        };
    }

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
        fonts: &FontSet,
    ) -> Self {
        let overlay_window = image_ops::build_overlay_window(tape_a);
        let scene_foreground = image_ops::build_cassette_foreground(tape_base, &overlay_window);
        let wheel_frames = image_ops::build_rotated_frames(wheel, ROTATION_FRAME_COUNT);
        let taperoll_cache =
            image_ops::build_taperoll_frame_cache(taperoll, TAPEROLL_FRAME_COUNT);

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
            requested_cover_url: None,
            applied_cover_url: None,
        };

        rs.rebuild_base_scene(fonts);
        rs.rebuild_playing_scene_locked(None);
        rs.rebuild_waiting_scene(fonts);
        rs
    }

    /// Draw the base scene (hint labels at bottom).
    fn rebuild_base_scene(&mut self, fonts: &FontSet) {
        drawing::clear_buffer(&mut self.scene_base, 0, 0, 0, 255);

        let hint_labels = [
            "PREV [\u{2190}]",
            "NEXT [\u{2192}]",
            "VOL+ [\u{2191}]",
            "VOL- [\u{2193}]",
            "PLAY / PAUSE [A]",
            "EXIT [B]",
        ];

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
                &mut self.scene_base,
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

    /// Rebuild the playing scene with optional cover art.
    pub fn rebuild_playing_scene(&mut self, cover: Option<&RgbaImage>) {
        self.rebuild_playing_scene_locked(cover);
    }

    fn rebuild_playing_scene_locked(&mut self, cover: Option<&RgbaImage>) {
        self.scene_playing.copy_from_slice(&self.scene_base);
        self.scene_cover = cover.map(|img| {
            image_ops::build_masked_cover(img, self.cover_mask.as_ref())
        });
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

        // Draw "Waiting..." message
        let msg = "Waiting for Spotify Connect...";
        let exit_hint = "EXIT [B]";

        // Clear hints area and draw centered text
        drawing::fill_rect(
            &mut self.scene_waiting,
            0,
            HINTS_BASELINE_Y - 28,
            SCREEN_W as i32,
            48,
            0,
            0,
            0,
            255,
        );

        let msg_w = fonts.measure_text(msg, fonts.scale_large);
        fonts.draw_text(
            &mut self.scene_waiting,
            msg,
            SCREEN_W as i32 / 2 - msg_w / 2,
            STATUS_BASELINE_Y,
            255,
            255,
            255,
            fonts.scale_large,
        );

        let hint_w = fonts.measure_text(exit_hint, fonts.scale_small);
        fonts.draw_text(
            &mut self.scene_waiting,
            exit_hint,
            SCREEN_W as i32 / 2 - hint_w / 2,
            HINTS_BASELINE_Y,
            255,
            255,
            255,
            fonts.scale_small,
        );

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
    let (paused, connected, position, duration, wheel_angle, _soundwave_bars) = {
        let st = app_state.lock().unwrap();
        (
            st.paused,
            st.connected,
            st.position,
            st.duration,
            st.wheel_angle,
            st.soundwave_bars,
        )
    };

    if !connected {
        back_buf.copy_from_slice(&render_state.scene_waiting);
        return;
    }

    // Dirty rects
    let dirty_rects: [(i32, i32, i32, i32); 3] = [
        (88, 64, 536, 520),    // left roll
        (488, 64, 936, 520),   // right roll
        (0, 620, SCREEN_W as i32, 690), // info bar
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

    // Draw status indicator
    let indicator = if paused {
        &render_state.img_paused
    } else {
        &render_state.img_playing
    };
    if let Some(img) = indicator {
        drawing::draw_image_alpha(back_buf, img, STATUS_LAMP_X, STATUS_LAMP_Y);
    } else {
        drawing::draw_status_dot(back_buf, STATUS_DOT_X, STATUS_DOT_Y);
    }

    // Status text needs fonts — we'll use the global font set
    // (handled by the render_loop caller)

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
) {
    let mut last_frame = Instant::now();
    let mut last_connected = false;
    let mut animation_mode = AnimationMode::new();

    loop {
        if quit.load(Ordering::Relaxed) {
            return;
        }

        let now = Instant::now();
        let dt = now.duration_since(last_frame);
        last_frame = now;

        let (connected, paused, dirty);
        {
            let mut st = app_state.lock().unwrap();
            connected = st.connected;
            paused = st.paused;
            dirty = st.render_dirty;

            if st.connected && !st.paused {
                st.wheel_angle = (st.wheel_angle
                    + 2.0 * std::f64::consts::PI * dt.as_secs_f64()
                        / WHEEL_ROTATION_PERIOD.as_secs_f64())
                    % (2.0 * std::f64::consts::PI);
            }

            if st.connected && !st.paused && st.duration > 0 {
                st.position += dt.as_millis() as i64;
                if st.position > st.duration {
                    st.position = st.duration;
                }
                st.last_pos_time = now;
            }
        }

        let full_redraw = {
            let mut rs = render_state.lock().unwrap();
            sync_scene_mode(&mut rs, last_connected, connected);
            rs.full_redraw
        };
        last_connected = connected;
        let plan = frame_plan(
            connected,
            paused,
            dirty,
            full_redraw,
            animation_mode.target_fps(),
        );

        if !plan.should_render {
            if !connected || paused {
                animation_mode.reset(now);
            }
            std::thread::sleep(plan.sleep);
            continue;
        }

        let render_started = Instant::now();
        if !connected {
            let mut rs = render_state.lock().unwrap();
            if rs.full_redraw || dirty {
                back_buf.copy_from_slice(&rs.scene_waiting);
                fb.swap_buffers(back_buf);
                rs.full_redraw = false;
            }
            drop(rs);
            app_state.lock().unwrap().render_dirty = false;
        } else {
            let mut rs = render_state.lock().unwrap();
            let full_redraw = rs.full_redraw;
            render(back_buf, &app_state, &mut rs);
            drop(rs);

            // Draw text overlay (status + time remaining)
            {
                let st = app_state.lock().unwrap();
                let status = if st.paused { "PAUSED" } else { "PLAYING" };
                fonts.draw_text(
                    back_buf,
                    status,
                    STATUS_TEXT_X,
                    STATUS_BASELINE_Y,
                    255,
                    255,
                    255,
                    fonts.scale_large,
                );

                let time_remaining = animation::format_duration(st.duration - st.position);
                let tr_w = fonts.measure_text(&time_remaining, fonts.scale_large);
                fonts.draw_text(
                    back_buf,
                    &time_remaining,
                    SCREEN_W as i32 - 28 - tr_w,
                    STATUS_BASELINE_Y,
                    255,
                    255,
                    255,
                    fonts.scale_large,
                );
            }

            if full_redraw {
                fb.swap_buffers(back_buf);
            } else {
                let dirty_rects: [(usize, usize, usize, usize); 3] = [
                    (88, 64, 536, 520),
                    (488, 64, 936, 520),
                    (0, 620, SCREEN_W, 690),
                ];
                for (x1, y1, x2, y2) in dirty_rects {
                    fb.copy_rect(back_buf, x1, y1, x2, y2);
                }
            }

            app_state.lock().unwrap().render_dirty = false;
        }

        if connected && !paused {
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
            requested_cover_url: None,
            applied_cover_url: None,
        }
    }

    #[test]
    fn frame_plan_skips_paused_frames_without_dirty_state() {
        let plan = frame_plan(true, true, false, false, BASE_ANIM_FPS);
        assert!(!plan.should_render);
        assert_eq!(plan.sleep, Duration::from_millis(100));
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
    fn animation_mode_stays_at_thirty_fps_after_sustained_fast_frames() {
        let mut mode = AnimationMode::new();
        let now = Instant::now();

        for i in 0..120 {
            mode.record_render(Duration::from_millis(6), now + Duration::from_millis(i * 33));
        }

        assert_eq!(mode.target_fps(), BASE_ANIM_FPS);
    }

    #[test]
    fn animation_mode_stays_at_thirty_fps_after_slow_frames() {
        let mut mode = AnimationMode::new();
        let now = Instant::now();

        for i in 0..120 {
            mode.record_render(Duration::from_millis(6), now + Duration::from_millis(i * 33));
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
        assert_eq!(rs.requested_cover_url.as_deref(), Some("https://img/cover-b"));
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
        }

        let mut rs = empty_render_state();
        let mut foreground = RgbaImage::new((COVER_X - TAPE_BASE_X + 1) as u32, (COVER_Y - TAPE_BASE_Y + 1) as u32);
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
