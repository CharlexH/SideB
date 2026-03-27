use crate::constants::*;

/// Reset all soundwave bars to idle minimum.
pub fn reset_soundwave_idle(bars: &mut [f64; 24], goals: &mut [f64; 24]) {
    for i in 0..24 {
        bars[i] = SOUNDWAVE_MIN_HEIGHT;
        goals[i] = SOUNDWAVE_MIN_HEIGHT;
    }
}

/// Set all soundwave goals to idle minimum.
pub fn set_soundwave_idle_goal(goals: &mut [f64; 24]) {
    for goal in goals.iter_mut() {
        *goal = SOUNDWAVE_MIN_HEIGHT;
    }
}

/// Refresh soundwave goals using sine-based animation.
pub fn refresh_soundwave_goal(goals: &mut [f64; 24], now_nanos: i128) {
    let phase_base = now_nanos as f64 / 1_000_000_000.0;
    for (i, goal) in goals.iter_mut().enumerate() {
        let phase = phase_base * 3.2 + i as f64 * 0.45;
        let value = 0.5 + 0.5 * (phase + 0.8 * (phase * 0.73).sin()).sin();
        *goal = SOUNDWAVE_MIN_HEIGHT + value * (SOUNDWAVE_MAX_HEIGHT - SOUNDWAVE_MIN_HEIGHT);
    }
}

/// Ease soundwave bars toward their goals.
pub fn step_soundwave(bars: &mut [f64; 24], goals: &[f64; 24], ease: f64) {
    for i in 0..24 {
        bars[i] += (goals[i] - bars[i]) * ease;
    }
}

/// Format duration in milliseconds as "M:SS".
pub fn format_duration(ms: i64) -> String {
    let ms = ms.max(0);
    let total_sec = ms / 1000;
    format!("{}:{:02}", total_sec / 60, total_sec % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(61000), "1:01");
        assert_eq!(format_duration(326080), "5:26");
        assert_eq!(format_duration(-100), "0:00");
    }
}
