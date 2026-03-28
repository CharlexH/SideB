use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::favorites::FavoriteEntry;

/// Manages local audio playback via ffmpeg | aplay subprocess pipeline.
pub struct LocalPlayer {
    ffmpeg_child: Option<Child>,
    aplay_child: Option<Child>,
    current_entry: Option<FavoriteEntry>,
    playlist: Vec<FavoriteEntry>,
    playlist_index: usize,
    start_time: Instant,
    paused: bool,
    paused_elapsed: Duration,
}

impl LocalPlayer {
    pub fn new() -> Self {
        Self {
            ffmpeg_child: None,
            aplay_child: None,
            current_entry: None,
            playlist: Vec::new(),
            playlist_index: 0,
            start_time: Instant::now(),
            paused: false,
            paused_elapsed: Duration::ZERO,
        }
    }

    /// Start shuffled playback of a list of downloaded favorites.
    pub fn start_shuffled(&mut self, entries: Vec<FavoriteEntry>) {
        if entries.is_empty() {
            return;
        }
        let mut playlist = entries;
        shuffle(&mut playlist);
        self.playlist = playlist;
        self.playlist_index = 0;
        self.play_current();
    }

    /// Start shuffled playback, ensuring the given URI plays first.
    pub fn start_shuffled_with_first(&mut self, entries: Vec<FavoriteEntry>, first_uri: &str) {
        if entries.is_empty() {
            return;
        }
        let mut playlist = entries;
        shuffle(&mut playlist);
        // Move the requested track to index 0
        if let Some(idx) = playlist.iter().position(|e| e.uri == first_uri) {
            playlist.swap(0, idx);
        }
        self.playlist = playlist;
        self.playlist_index = 0;
        self.play_current();
    }

    /// Play the track at the current playlist index.
    /// On failure (missing file, spawn error), skips forward up to playlist.len() times
    /// to find a playable track. Stops if none found.
    fn play_current(&mut self) {
        self.stop_subprocess();
        self.current_entry = None;

        if self.playlist.is_empty() {
            return;
        }

        let max_skips = self.playlist.len();
        for skip in 0..max_skips {
            let idx = (self.playlist_index + skip) % self.playlist.len();
            let entry = self.playlist[idx].clone();

            let file_path = match entry.file_path {
                Some(ref fp) => fp.clone(),
                None => {
                    eprintln!("local_player: no file_path for {}, skipping", entry.name);
                    continue;
                }
            };

            if !std::path::Path::new(&file_path).exists() {
                eprintln!("local_player: file not found: {}, skipping", file_path);
                continue;
            }

            eprintln!("local_player: playing {} - {}", entry.artist, entry.name);

            match spawn_pipeline(&file_path) {
                Ok((ffmpeg, aplay)) => {
                    self.ffmpeg_child = Some(ffmpeg);
                    self.aplay_child = Some(aplay);
                    self.current_entry = Some(entry);
                    self.playlist_index = idx;
                    self.start_time = Instant::now();
                    self.paused = false;
                    self.paused_elapsed = Duration::ZERO;
                    return;
                }
                Err(e) => {
                    eprintln!("local_player: spawn error for {}: {e}", entry.name);
                    continue;
                }
            }
        }

        eprintln!("local_player: no playable track found in playlist");
    }

    /// Play a specific entry (for playlist selection).
    pub fn play_entry(&mut self, entry: &FavoriteEntry) {
        // Find in playlist or add it
        if let Some(idx) = self.playlist.iter().position(|e| e.uri == entry.uri) {
            self.playlist_index = idx;
        } else {
            self.playlist.push(entry.clone());
            self.playlist_index = self.playlist.len() - 1;
        }
        self.play_current();
    }

    pub fn pause(&mut self) {
        if self.paused {
            return;
        }
        self.paused = true;
        self.paused_elapsed += self.start_time.elapsed();
        send_signal(&self.ffmpeg_child, libc::SIGSTOP);
        send_signal(&self.aplay_child, libc::SIGSTOP);
        eprintln!("local_player: paused");
    }

    pub fn resume(&mut self) {
        if !self.paused {
            return;
        }
        send_signal(&self.ffmpeg_child, libc::SIGCONT);
        send_signal(&self.aplay_child, libc::SIGCONT);
        self.paused = false;
        self.start_time = Instant::now();
        eprintln!("local_player: resumed");
    }

    pub fn toggle_pause(&mut self) {
        if self.paused {
            self.resume();
        } else {
            self.pause();
        }
    }

    pub fn stop(&mut self) {
        self.stop_subprocess();
        self.current_entry = None;
        self.playlist.clear();
        self.playlist_index = 0;
        eprintln!("local_player: stopped");
    }

    /// Refresh the playlist with newly downloaded entries while preserving current position.
    /// New tracks are appended; removed tracks are pruned.
    pub fn refresh_playlist(&mut self, entries: Vec<FavoriteEntry>) {
        if self.playlist.is_empty() {
            // No active playlist — just replace
            self.playlist = entries;
            return;
        }

        let current_uri = self.playlist.get(self.playlist_index).map(|e| e.uri.clone());

        // Add new entries that aren't already in the playlist
        let existing_uris: std::collections::HashSet<String> =
            self.playlist.iter().map(|e| e.uri.clone()).collect();
        for entry in entries {
            if !existing_uris.contains(&entry.uri) {
                self.playlist.push(entry);
            }
        }

        // Restore index to current track
        if let Some(uri) = current_uri {
            if let Some(idx) = self.playlist.iter().position(|e| e.uri == uri) {
                self.playlist_index = idx;
            }
        }
    }

    pub fn next(&mut self) {
        if self.playlist.is_empty() {
            return;
        }
        self.playlist_index = (self.playlist_index + 1) % self.playlist.len();
        self.play_current();
    }

    pub fn prev(&mut self) {
        if self.playlist.is_empty() {
            return;
        }
        if self.playlist_index == 0 {
            self.playlist_index = self.playlist.len() - 1;
        } else {
            self.playlist_index -= 1;
        }
        self.play_current();
    }

    /// Check if the current track has finished playing.
    pub fn is_finished(&mut self) -> bool {
        if let Some(ref mut child) = self.ffmpeg_child {
            match child.try_wait() {
                Ok(Some(_)) => true,
                Ok(None) => false,
                Err(_) => true,
            }
        } else {
            // No process means not playing
            self.current_entry.is_some()
        }
    }

    /// Auto-advance to next track if current finished.
    /// Returns true if a new track started.
    pub fn check_and_advance(&mut self) -> bool {
        if self.paused || self.current_entry.is_none() {
            return false;
        }
        if self.is_finished() {
            self.next();
            return true;
        }
        false
    }

    /// Current playback position in milliseconds (estimated from wall clock).
    pub fn position_ms(&self) -> i64 {
        if self.current_entry.is_none() {
            return 0;
        }
        let elapsed = if self.paused {
            self.paused_elapsed
        } else {
            self.paused_elapsed + self.start_time.elapsed()
        };
        elapsed.as_millis() as i64
    }

    pub fn current_entry(&self) -> Option<&FavoriteEntry> {
        self.current_entry.as_ref()
    }

    pub fn is_playing(&self) -> bool {
        self.current_entry.is_some() && !self.paused
    }

    pub fn is_paused(&self) -> bool {
        self.current_entry.is_some() && self.paused
    }

    pub fn is_active(&self) -> bool {
        self.current_entry.is_some()
    }

    fn stop_subprocess(&mut self) {
        // Resume first if paused, so SIGKILL can be delivered
        if self.paused {
            send_signal(&self.ffmpeg_child, libc::SIGCONT);
            send_signal(&self.aplay_child, libc::SIGCONT);
        }

        if let Some(ref mut child) = self.ffmpeg_child {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(ref mut child) = self.aplay_child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.ffmpeg_child = None;
        self.aplay_child = None;
    }
}

impl Drop for LocalPlayer {
    fn drop(&mut self) {
        self.stop_subprocess();
    }
}

/// Spawn the ffmpeg → aplay pipeline for a given audio file.
fn spawn_pipeline(file_path: &str) -> Result<(Child, Child), Box<dyn std::error::Error>> {
    let mut ffmpeg = Command::new("ffmpeg")
        .args([
            "-i",
            file_path,
            "-f",
            "s16le",
            "-ar",
            "44100",
            "-ac",
            "2",
            "-loglevel",
            "quiet",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()?;

    let ffmpeg_stdout = ffmpeg
        .stdout
        .take()
        .ok_or("failed to capture ffmpeg stdout")?;

    let aplay = Command::new("aplay")
        .args(["-f", "S16_LE", "-r", "44100", "-c", "2", "-q"])
        .stdin(ffmpeg_stdout)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    Ok((ffmpeg, aplay))
}

/// Send a signal to a child process.
fn send_signal(child: &Option<Child>, signal: libc::c_int) {
    if let Some(ref child) = child {
        let pid = child.id() as libc::pid_t;
        unsafe {
            libc::kill(pid, signal);
        }
    }
}

/// Fisher-Yates shuffle using a simple xorshift PRNG.
fn shuffle<T>(slice: &mut [T]) {
    if slice.len() <= 1 {
        return;
    }
    let mut rng = Instant::now().elapsed().as_nanos() as u64;
    if rng == 0 {
        rng = 42;
    }
    for i in (1..slice.len()).rev() {
        // xorshift64
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let j = (rng as usize) % (i + 1);
        slice.swap(i, j);
    }
}
