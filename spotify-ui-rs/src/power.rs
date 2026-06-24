use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::AppState;
use crate::download::DownloadProgressMap;
use crate::local_player::LocalPlayer;
use crate::mode::AppMode;

pub const SLEEP_GRACE: Duration = Duration::from_secs(30);
const MONITOR_INTERVAL: Duration = Duration::from_secs(1);
const KEEPALIVE_PATHS: [&str; 2] = ["/tmp/stay_awake", "/tmp/stay_alive"];
const NEXTUI_SUSPEND_CANDIDATES: [&str; 8] = [
    "/mnt/SDCARD/.system/tg5040/bin/suspend",
    "/mnt/SDCARD/.system/tg5050/bin/suspend",
    "/mnt/SDCARD/System/tg5040/bin/suspend",
    "/mnt/SDCARD/SYSTEM/tg5040/bin/suspend",
    "/sdcard/System/tg5040/bin/suspend",
    "/sdcard/SYSTEM/tg5040/bin/suspend",
    "/usr/bin/suspend",
    "/bin/suspend",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotifyPowerState {
    Unknown,
    Playing,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackPowerState {
    pub spotify: SpotifyPowerState,
    pub local_active: bool,
    pub local_paused: bool,
    pub downloads_active: bool,
    pub stop_eligible: bool,
}

impl PlaybackPowerState {
    fn permits_sleep(self) -> bool {
        self.spotify == SpotifyPowerState::Stopped
            && !self.local_active
            && !self.local_paused
            && !self.downloads_active
            && self.stop_eligible
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepDecision {
    Idle,
    Armed,
    Cancelled,
    Ready,
}

pub struct SleepGate {
    armed_at: Option<Instant>,
    grace: Duration,
}

impl SleepGate {
    pub fn new(grace: Duration) -> Self {
        Self {
            armed_at: None,
            grace,
        }
    }

    pub fn is_armed(&self) -> bool {
        self.armed_at.is_some()
    }

    pub fn disarm(&mut self) {
        self.armed_at = None;
    }

    pub fn update(&mut self, state: &PlaybackPowerState, now: Instant) -> SleepDecision {
        if !state.permits_sleep() {
            if self.armed_at.take().is_some() {
                return SleepDecision::Cancelled;
            }
            return SleepDecision::Idle;
        }

        let armed_at = match self.armed_at {
            Some(armed_at) => armed_at,
            None => {
                self.armed_at = Some(now);
                return SleepDecision::Armed;
            }
        };

        if now.saturating_duration_since(armed_at) >= self.grace {
            SleepDecision::Ready
        } else {
            SleepDecision::Armed
        }
    }
}

pub fn run_sleep_monitor(
    app_state: Arc<Mutex<AppState>>,
    local_player: Arc<Mutex<LocalPlayer>>,
    download_progress: DownloadProgressMap,
    quit: Arc<AtomicBool>,
) {
    let mut gate = SleepGate::new(SLEEP_GRACE);

    while !quit.load(Ordering::Relaxed) {
        let now = Instant::now();
        let state = snapshot_power_state(&app_state, &local_player, &download_progress);

        match gate.update(&state, now) {
            SleepDecision::Ready => {
                let final_state =
                    snapshot_power_state(&app_state, &local_player, &download_progress);
                if final_state.permits_sleep() {
                    eprintln!("power: stopped playback confirmed, invoking NextUI suspend");
                    if let Err(err) = invoke_nextui_suspend() {
                        eprintln!("power: suspend failed: {err}");
                    } else {
                        eprintln!("power: resumed from suspend");
                    }
                } else {
                    eprintln!("power: suspend cancelled by final playback check: {final_state:?}");
                }
                gate.disarm();
            }
            SleepDecision::Cancelled => {
                eprintln!("power: sleep arm cancelled by playback/download activity");
            }
            SleepDecision::Armed | SleepDecision::Idle => {}
        }

        std::thread::sleep(MONITOR_INTERVAL);
    }
}

fn snapshot_power_state(
    app_state: &Arc<Mutex<AppState>>,
    local_player: &Arc<Mutex<LocalPlayer>>,
    download_progress: &DownloadProgressMap,
) -> PlaybackPowerState {
    let (mode, connected, paused, has_track, stop_eligible) = {
        let st = app_state.lock().unwrap();
        (
            st.mode,
            st.connected,
            st.paused,
            !st.current_track_uri.is_empty(),
            st.stop_to_sleep_eligible,
        )
    };

    let (player_playing, player_paused) = {
        let player = local_player.lock().unwrap();
        (player.is_playing(), player.is_paused())
    };

    let spotify = match mode {
        AppMode::Spotify if has_track => {
            if paused {
                SpotifyPowerState::Paused
            } else {
                SpotifyPowerState::Playing
            }
        }
        AppMode::Spotify if connected => SpotifyPowerState::Unknown,
        AppMode::Spotify | AppMode::Waiting | AppMode::Local => SpotifyPowerState::Stopped,
    };

    PlaybackPowerState {
        spotify,
        local_active: player_playing,
        local_paused: player_paused || (mode == AppMode::Local && paused && has_track),
        downloads_active: !download_progress.lock().unwrap().is_empty(),
        stop_eligible,
    }
}

fn invoke_nextui_suspend() -> io::Result<()> {
    let keepalive_paths = keepalive_paths();
    with_keepalive_released(&keepalive_paths, run_nextui_suspend)
}

fn keepalive_paths() -> Vec<PathBuf> {
    KEEPALIVE_PATHS.iter().map(PathBuf::from).collect()
}

fn run_nextui_suspend() -> io::Result<()> {
    run_nextui_suspend_from(&suspend_candidate_paths())
}

fn suspend_candidate_paths() -> Vec<PathBuf> {
    NEXTUI_SUSPEND_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .collect()
}

fn run_nextui_suspend_from(candidates: &[PathBuf]) -> io::Result<()> {
    let path = candidates
        .iter()
        .find(|path| path.exists())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "NextUI suspend script not found in known locations",
            )
        })?;

    eprintln!("power: running suspend script {}", path.display());
    let status = Command::new(path).status()?;
    if status.success() {
        return Ok(());
    }

    Err(io::Error::other(format!(
        "suspend script exited with {status}"
    )))
}

fn with_keepalive_released<F>(paths: &[PathBuf], action: F) -> io::Result<()>
where
    F: FnOnce() -> io::Result<()>,
{
    for path in paths {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => eprintln!("power: failed to remove {}: {err}", path.display()),
        }
    }

    let result = action();
    let mut restore_error = None;

    for path in paths {
        if let Some(parent) = path.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                eprintln!("power: failed to create {}: {err}", parent.display());
                restore_error.get_or_insert(err);
                continue;
            }
        }
        if let Err(err) = fs::File::create(path) {
            eprintln!("power: failed to restore {}: {err}", path.display());
            restore_error.get_or_insert(err);
        }
    }

    match (result, restore_error) {
        (Err(err), _) => Err(err),
        (Ok(()), Some(err)) => Err(err),
        (Ok(()), None) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("sideb-power-{name}-{suffix}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn stopped_state() -> PlaybackPowerState {
        PlaybackPowerState {
            spotify: SpotifyPowerState::Stopped,
            local_active: false,
            local_paused: false,
            downloads_active: false,
            stop_eligible: true,
        }
    }

    #[test]
    fn idle_waiting_without_a_stop_event_does_not_arm_sleep() {
        let mut gate = SleepGate::new(Duration::from_secs(1));
        let now = Instant::now();
        let state = PlaybackPowerState {
            stop_eligible: false,
            ..stopped_state()
        };

        assert_eq!(gate.update(&state, now), SleepDecision::Idle);
        assert!(!gate.is_armed());
    }

    #[test]
    fn stopped_playback_arms_then_becomes_ready_after_grace_period() {
        let mut gate = SleepGate::new(Duration::from_secs(5));
        let now = Instant::now();

        assert_eq!(gate.update(&stopped_state(), now), SleepDecision::Armed);
        assert_eq!(
            gate.update(&stopped_state(), now + Duration::from_secs(5)),
            SleepDecision::Ready
        );
    }

    #[test]
    fn paused_or_playing_playback_cancels_an_armed_sleep() {
        let mut gate = SleepGate::new(Duration::from_secs(5));
        let now = Instant::now();
        gate.update(&stopped_state(), now);

        let paused = PlaybackPowerState {
            spotify: SpotifyPowerState::Paused,
            ..stopped_state()
        };
        assert_eq!(
            gate.update(&paused, now + Duration::from_secs(1)),
            SleepDecision::Cancelled
        );
        assert!(!gate.is_armed());

        let playing = PlaybackPowerState {
            spotify: SpotifyPowerState::Playing,
            ..stopped_state()
        };
        assert_eq!(
            gate.update(&playing, now + Duration::from_secs(2)),
            SleepDecision::Idle
        );
    }

    #[test]
    fn downloads_or_local_playback_block_sleep() {
        let mut gate = SleepGate::new(Duration::from_secs(1));
        let now = Instant::now();

        let downloading = PlaybackPowerState {
            downloads_active: true,
            ..stopped_state()
        };
        assert_eq!(gate.update(&downloading, now), SleepDecision::Idle);

        let local_paused = PlaybackPowerState {
            local_paused: true,
            ..stopped_state()
        };
        assert_eq!(
            gate.update(&local_paused, now + Duration::from_secs(1)),
            SleepDecision::Idle
        );
    }

    #[test]
    fn keepalive_files_are_restored_after_suspend_attempt() {
        let dir = temp_dir("keepalive");
        let stay_awake = dir.join("stay_awake");
        let stay_alive = dir.join("stay_alive");
        fs::write(&stay_awake, "").unwrap();
        fs::write(&stay_alive, "").unwrap();

        with_keepalive_released(&[stay_awake.clone(), stay_alive.clone()], || {
            assert!(!stay_awake.exists());
            assert!(!stay_alive.exists());
            Ok(())
        })
        .unwrap();

        assert!(stay_awake.exists());
        assert!(stay_alive.exists());
    }

    #[test]
    fn missing_nextui_suspend_script_returns_not_found_without_path_fallback() {
        let dir = temp_dir("missing-suspend");
        let missing = dir.join("suspend");

        let err = run_nextui_suspend_from(&[missing]).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
