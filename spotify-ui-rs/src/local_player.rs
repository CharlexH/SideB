use std::ffi::{CStr, CString};
use std::io::Read;
use std::os::raw::{c_char, c_int, c_void};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::constants::FFMPEG_TRANSCODER_BIN;
use crate::favorites::FavoriteEntry;

const PCM_SAMPLE_RATE: i32 = 44_100;
const PCM_CHANNELS: usize = 2;
const PCM_RING_SECONDS: usize = 3;

#[derive(Debug)]
struct PcmRingBuffer {
    samples: Vec<i16>,
    read_pos: usize,
    write_pos: usize,
    available: usize,
}

impl PcmRingBuffer {
    fn new(capacity_samples: usize) -> Self {
        Self {
            samples: vec![0; capacity_samples.max(1)],
            read_pos: 0,
            write_pos: 0,
            available: 0,
        }
    }

    fn available_samples(&self) -> usize {
        self.available
    }

    fn write_samples(&mut self, input: &[i16]) -> usize {
        let writable = input.len().min(self.samples.len() - self.available);
        for sample in input.iter().take(writable) {
            self.samples[self.write_pos] = *sample;
            self.write_pos = (self.write_pos + 1) % self.samples.len();
        }
        self.available += writable;
        writable
    }

    fn read_samples(&mut self, output: &mut [i16]) -> usize {
        let readable = output.len().min(self.available);
        for out in output.iter_mut().take(readable) {
            *out = self.samples[self.read_pos];
            self.read_pos = (self.read_pos + 1) % self.samples.len();
        }
        self.available -= readable;
        for out in output.iter_mut().skip(readable) {
            *out = 0;
        }
        readable
    }

    fn clear(&mut self) {
        self.read_pos = 0;
        self.write_pos = 0;
        self.available = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalPlaybackError {
    MissingDecoder,
    SpawnFailed,
    MissingAudioPipe,
    FileMissing,
    NoPlayableTrack,
}

impl LocalPlaybackError {
    pub fn notice(self) -> &'static str {
        match self {
            Self::MissingDecoder => "Missing ffmpeg",
            Self::FileMissing => "File missing",
            Self::MissingAudioPipe | Self::NoPlayableTrack | Self::SpawnFailed => "Playback failed",
        }
    }
}

enum PlaybackSession {
    Sdl(SdlPlayback),
}

impl PlaybackSession {
    fn label(&self) -> &'static str {
        match self {
            Self::Sdl(_) => "sdl",
        }
    }

    fn pause(&mut self) {
        match self {
            Self::Sdl(session) => session.pause(),
        }
    }

    fn resume(&mut self) {
        match self {
            Self::Sdl(session) => session.resume(),
        }
    }

    fn stop(&mut self) {
        match self {
            Self::Sdl(session) => session.stop(),
        }
    }

    fn is_finished(&mut self) -> bool {
        match self {
            Self::Sdl(session) => session.is_finished(),
        }
    }

    fn position_ms(&self) -> Option<i64> {
        match self {
            Self::Sdl(session) => Some(session.position_ms()),
        }
    }
}

#[repr(C)]
struct SdlAudioSpec {
    freq: c_int,
    format: u16,
    channels: u8,
    silence: u8,
    samples: u16,
    padding: u16,
    size: u32,
    callback: Option<unsafe extern "C" fn(*mut c_void, *mut u8, c_int)>,
    userdata: *mut c_void,
}

type SdlInitSubSystem = unsafe extern "C" fn(u32) -> c_int;
type SdlQuitSubSystem = unsafe extern "C" fn(u32);
type SdlOpenAudioDevice = unsafe extern "C" fn(
    *const c_char,
    c_int,
    *const SdlAudioSpec,
    *mut SdlAudioSpec,
    c_int,
) -> u32;
type SdlPauseAudioDevice = unsafe extern "C" fn(u32, c_int);
type SdlCloseAudioDevice = unsafe extern "C" fn(u32);
type SdlGetError = unsafe extern "C" fn() -> *const c_char;

struct SdlLibrary {
    handle: *mut c_void,
    init_sub_system: SdlInitSubSystem,
    quit_sub_system: SdlQuitSubSystem,
    open_audio_device: SdlOpenAudioDevice,
    pause_audio_device: SdlPauseAudioDevice,
    close_audio_device: SdlCloseAudioDevice,
    get_error: SdlGetError,
}

unsafe impl Send for SdlLibrary {}
unsafe impl Sync for SdlLibrary {}

impl SdlLibrary {
    fn load() -> Result<Arc<Self>, LocalPlaybackError> {
        const CANDIDATES: [&str; 3] = [
            "/usr/trimui/lib/libSDL2-2.0.so.0",
            "libSDL2-2.0.so.0",
            "libSDL2.so",
        ];

        for candidate in CANDIDATES {
            let Ok(path) = CString::new(candidate) else {
                continue;
            };
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                eprintln!(
                    "local_player: SDL load failed candidate={candidate}: {}",
                    dynamic_loader_error()
                );
                continue;
            }

            let library = unsafe {
                Self {
                    handle,
                    init_sub_system: load_sdl_symbol(handle, "SDL_InitSubSystem")?,
                    quit_sub_system: load_sdl_symbol(handle, "SDL_QuitSubSystem")?,
                    open_audio_device: load_sdl_symbol(handle, "SDL_OpenAudioDevice")?,
                    pause_audio_device: load_sdl_symbol(handle, "SDL_PauseAudioDevice")?,
                    close_audio_device: load_sdl_symbol(handle, "SDL_CloseAudioDevice")?,
                    get_error: load_sdl_symbol(handle, "SDL_GetError")?,
                }
            };
            eprintln!("local_player: SDL loaded from {candidate}");
            return Ok(Arc::new(library));
        }

        eprintln!("local_player: SDL library not found");
        Err(LocalPlaybackError::SpawnFailed)
    }

    fn error_string(&self) -> String {
        let error = unsafe { (self.get_error)() };
        if error.is_null() {
            return "unknown".to_string();
        }
        unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    }
}

fn dynamic_loader_error() -> String {
    let error = unsafe { libc::dlerror() };
    if error.is_null() {
        return "unknown".to_string();
    }
    unsafe { CStr::from_ptr(error) }
        .to_string_lossy()
        .into_owned()
}

impl Drop for SdlLibrary {
    fn drop(&mut self) {
        unsafe {
            libc::dlclose(self.handle);
        }
    }
}

unsafe fn load_sdl_symbol<T: Copy>(
    handle: *mut c_void,
    name: &str,
) -> Result<T, LocalPlaybackError> {
    let symbol_name = CString::new(name).map_err(|_| LocalPlaybackError::SpawnFailed)?;
    let symbol = libc::dlsym(handle, symbol_name.as_ptr());
    if symbol.is_null() {
        eprintln!("local_player: SDL missing symbol {name}");
        return Err(LocalPlaybackError::SpawnFailed);
    }
    Ok(std::mem::transmute_copy(&symbol))
}

struct SdlShared {
    ring: Mutex<PcmRingBuffer>,
    producer_eof: AtomicBool,
    stop_requested: AtomicBool,
    output_samples: AtomicU64,
    underruns: AtomicU64,
}

impl SdlShared {
    fn new() -> Self {
        Self {
            ring: Mutex::new(PcmRingBuffer::new(
                PCM_SAMPLE_RATE as usize * PCM_CHANNELS * PCM_RING_SECONDS,
            )),
            producer_eof: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            output_samples: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
        }
    }
}

struct SdlCallbackState {
    shared: Arc<SdlShared>,
}

struct SdlAudioDevice {
    lib: Arc<SdlLibrary>,
    id: u32,
}

impl SdlAudioDevice {
    fn open(
        lib: Arc<SdlLibrary>,
        shared: Arc<SdlShared>,
    ) -> Result<(Self, Box<SdlCallbackState>, SdlAudioSpec), LocalPlaybackError> {
        const SDL_INIT_AUDIO: u32 = 0x0000_0010;
        const AUDIO_S16LSB: u16 = 0x8010;

        let init_status = unsafe { (lib.init_sub_system)(SDL_INIT_AUDIO) };
        if init_status != 0 {
            eprintln!(
                "local_player: SDL audio init failed: {}",
                lib.error_string()
            );
            return Err(LocalPlaybackError::SpawnFailed);
        }

        let mut callback_state = Box::new(SdlCallbackState { shared });
        let want = SdlAudioSpec {
            freq: PCM_SAMPLE_RATE,
            format: AUDIO_S16LSB,
            channels: PCM_CHANNELS as u8,
            silence: 0,
            samples: 2048,
            padding: 0,
            size: 0,
            callback: Some(sdl_audio_callback),
            userdata: callback_state.as_mut() as *mut SdlCallbackState as *mut c_void,
        };
        let mut have = SdlAudioSpec {
            freq: 0,
            format: 0,
            channels: 0,
            silence: 0,
            samples: 0,
            padding: 0,
            size: 0,
            callback: None,
            userdata: std::ptr::null_mut(),
        };

        let id = unsafe { (lib.open_audio_device)(std::ptr::null(), 0, &want, &mut have, 0) };
        if id == 0 {
            eprintln!(
                "local_player: SDL open audio failed: {}",
                lib.error_string()
            );
            unsafe { (lib.quit_sub_system)(SDL_INIT_AUDIO) };
            return Err(LocalPlaybackError::SpawnFailed);
        }

        eprintln!(
            "local_player: SDL audio opened freq={} channels={} samples={}",
            have.freq, have.channels, have.samples
        );
        Ok((Self { lib, id }, callback_state, have))
    }

    fn pause(&self, paused: bool) {
        unsafe {
            (self.lib.pause_audio_device)(self.id, if paused { 1 } else { 0 });
        }
    }
}

impl Drop for SdlAudioDevice {
    fn drop(&mut self) {
        const SDL_INIT_AUDIO: u32 = 0x0000_0010;
        unsafe {
            (self.lib.pause_audio_device)(self.id, 1);
            (self.lib.close_audio_device)(self.id);
            (self.lib.quit_sub_system)(SDL_INIT_AUDIO);
        }
    }
}

struct SdlPlayback {
    ffmpeg_child: Option<Child>,
    reader_thread: Option<JoinHandle<()>>,
    shared: Arc<SdlShared>,
    device: Option<SdlAudioDevice>,
    _callback_state: Box<SdlCallbackState>,
    sample_rate: i32,
    channels: usize,
}

impl SdlPlayback {
    fn start(file_path: &str) -> Result<Self, LocalPlaybackError> {
        let lib = SdlLibrary::load()?;
        let shared = Arc::new(SdlShared::new());
        let (device, callback_state, obtained) =
            SdlAudioDevice::open(Arc::clone(&lib), Arc::clone(&shared))?;
        let (ffmpeg_child, stdout) = spawn_ffmpeg_pcm(file_path)?;
        let reader_shared = Arc::clone(&shared);
        let reader_thread = thread::spawn(move || read_pcm_stdout(stdout, reader_shared));

        device.pause(false);
        eprintln!(
            "local_player: SDL pipeline ready ffmpeg_pid={} underruns=0",
            ffmpeg_child.id()
        );

        Ok(Self {
            ffmpeg_child: Some(ffmpeg_child),
            reader_thread: Some(reader_thread),
            shared,
            device: Some(device),
            _callback_state: callback_state,
            sample_rate: obtained.freq.max(1),
            channels: (obtained.channels as usize).max(1),
        })
    }

    fn pause(&self) {
        if let Some(device) = self.device.as_ref() {
            device.pause(true);
        }
    }

    fn resume(&self) {
        if let Some(device) = self.device.as_ref() {
            device.pause(false);
        }
    }

    fn stop(&mut self) {
        self.shared.stop_requested.store(true, Ordering::Relaxed);
        if let Some(device) = self.device.take() {
            device.pause(true);
            drop(device);
        }
        if let Some(ref mut child) = self.ffmpeg_child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.ffmpeg_child = None;
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
        let underruns = self.shared.underruns.load(Ordering::Relaxed);
        if underruns > 0 {
            eprintln!("local_player: SDL underruns={underruns}");
        }
    }

    fn is_finished(&self) -> bool {
        if !self.shared.producer_eof.load(Ordering::Relaxed) {
            return false;
        }
        self.shared
            .ring
            .lock()
            .map(|ring| ring.available_samples() == 0)
            .unwrap_or(true)
    }

    fn position_ms(&self) -> i64 {
        let samples = self.shared.output_samples.load(Ordering::Relaxed);
        let frames = samples / self.channels.max(1) as u64;
        ((frames * 1000) / self.sample_rate.max(1) as u64) as i64
    }
}

impl Drop for SdlPlayback {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Manages local audio playback via ffmpeg-lite PCM decoding and SDL output.
pub struct LocalPlayer {
    session: Option<PlaybackSession>,
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
            session: None,
            current_entry: None,
            playlist: Vec::new(),
            playlist_index: 0,
            start_time: Instant::now(),
            paused: false,
            paused_elapsed: Duration::ZERO,
        }
    }

    /// Start shuffled playback of a list of downloaded favorites.
    pub fn start_shuffled(
        &mut self,
        entries: Vec<FavoriteEntry>,
    ) -> Result<(), LocalPlaybackError> {
        if entries.is_empty() {
            return Err(LocalPlaybackError::NoPlayableTrack);
        }
        let mut playlist = entries;
        shuffle(&mut playlist);
        self.playlist = playlist;
        self.playlist_index = 0;
        self.play_current()
    }

    /// Start shuffled playback, ensuring the given URI plays first.
    pub fn start_shuffled_with_first(
        &mut self,
        entries: Vec<FavoriteEntry>,
        first_uri: &str,
    ) -> Result<(), LocalPlaybackError> {
        if entries.is_empty() {
            return Err(LocalPlaybackError::NoPlayableTrack);
        }
        let mut playlist = entries;
        shuffle(&mut playlist);
        // Move the requested track to index 0
        if let Some(idx) = playlist.iter().position(|e| e.uri == first_uri) {
            playlist.swap(0, idx);
        }
        self.playlist = playlist;
        self.playlist_index = 0;
        self.play_current()
    }

    /// Play the track at the current playlist index.
    /// On failure (missing file, spawn error), skips forward up to playlist.len() times
    /// to find a playable track. Stops if none found.
    fn play_current(&mut self) -> Result<(), LocalPlaybackError> {
        self.stop_subprocess();
        self.current_entry = None;

        if self.playlist.is_empty() {
            return Err(LocalPlaybackError::NoPlayableTrack);
        }

        let max_skips = self.playlist.len();
        let mut last_error = None;
        for skip in 0..max_skips {
            let idx = (self.playlist_index + skip) % self.playlist.len();
            let entry = self.playlist[idx].clone();

            let file_path = match entry.file_path {
                Some(ref fp) => fp.clone(),
                None => {
                    eprintln!(
                        "local_player: skip idx={}/{} uri={} track={} - {} reason=no file_path",
                        idx + 1,
                        self.playlist.len(),
                        entry.uri,
                        entry.artist,
                        entry.name
                    );
                    last_error = Some(LocalPlaybackError::FileMissing);
                    continue;
                }
            };

            if !std::path::Path::new(&file_path).exists() {
                eprintln!(
                    "local_player: skip idx={}/{} uri={} path={} reason=file missing",
                    idx + 1,
                    self.playlist.len(),
                    entry.uri,
                    file_path
                );
                last_error = Some(LocalPlaybackError::FileMissing);
                continue;
            }

            eprintln!(
                "local_player: starting idx={}/{} uri={} track={} - {} path={}",
                idx + 1,
                self.playlist.len(),
                entry.uri,
                entry.artist,
                entry.name,
                file_path
            );

            match spawn_playback_session(&file_path) {
                Ok(session) => {
                    self.session = Some(session);
                    self.current_entry = Some(entry);
                    self.playlist_index = idx;
                    self.start_time = Instant::now();
                    self.paused = false;
                    self.paused_elapsed = Duration::ZERO;
                    eprintln!(
                        "local_player: pipeline ready uri={} backend={}",
                        self.current_entry
                            .as_ref()
                            .map(|entry| entry.uri.as_str())
                            .unwrap_or("unknown"),
                        self.session
                            .as_ref()
                            .map(|session| session.label())
                            .unwrap_or("none")
                    );
                    return Ok(());
                }
                Err(e) => {
                    eprintln!(
                        "local_player: spawn error idx={}/{} uri={} track={} - {} error={e:?}",
                        idx + 1,
                        self.playlist.len(),
                        entry.uri,
                        entry.artist,
                        entry.name
                    );
                    last_error = Some(e);
                    continue;
                }
            }
        }

        eprintln!(
            "local_player: no playable track found in playlist size={}",
            self.playlist.len()
        );
        Err(last_error.unwrap_or(LocalPlaybackError::NoPlayableTrack))
    }

    /// Play a specific entry (for playlist selection).
    pub fn play_entry(&mut self, entry: &FavoriteEntry) -> Result<(), LocalPlaybackError> {
        // Find in playlist or add it
        if let Some(idx) = self.playlist.iter().position(|e| e.uri == entry.uri) {
            self.playlist_index = idx;
        } else {
            self.playlist.push(entry.clone());
            self.playlist_index = self.playlist.len() - 1;
        }
        self.play_current()
    }

    pub fn pause(&mut self) {
        if self.paused {
            return;
        }
        self.paused = true;
        self.paused_elapsed += self.start_time.elapsed();
        if let Some(session) = self.session.as_mut() {
            session.pause();
        }
        eprintln!(
            "local_player: paused track={}",
            current_track_label(self.current_entry.as_ref())
        );
    }

    pub fn resume(&mut self) {
        if !self.paused {
            return;
        }
        if let Some(session) = self.session.as_mut() {
            session.resume();
        }
        self.paused = false;
        self.start_time = Instant::now();
        eprintln!(
            "local_player: resumed track={}",
            current_track_label(self.current_entry.as_ref())
        );
    }

    pub fn toggle_pause(&mut self) {
        if self.paused {
            self.resume();
        } else {
            self.pause();
        }
    }

    pub fn stop(&mut self) {
        let stopped_track = current_track_label(self.current_entry.as_ref());
        self.stop_subprocess();
        self.current_entry = None;
        self.playlist.clear();
        self.playlist_index = 0;
        eprintln!("local_player: stopped track={stopped_track}");
    }

    /// Refresh the playlist with newly downloaded entries while preserving current position.
    /// New tracks are appended; removed tracks are pruned.
    pub fn refresh_playlist(&mut self, entries: Vec<FavoriteEntry>) {
        if self.playlist.is_empty() {
            // No active playlist — just replace
            self.playlist = entries;
            return;
        }

        let current_uri = self
            .playlist
            .get(self.playlist_index)
            .map(|e| e.uri.clone());

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

    pub fn next(&mut self) -> Result<(), LocalPlaybackError> {
        if self.playlist.is_empty() {
            return Err(LocalPlaybackError::NoPlayableTrack);
        }
        self.playlist_index = (self.playlist_index + 1) % self.playlist.len();
        self.play_current()
    }

    pub fn prev(&mut self) -> Result<(), LocalPlaybackError> {
        if self.playlist.is_empty() {
            return Err(LocalPlaybackError::NoPlayableTrack);
        }
        if self.playlist_index == 0 {
            self.playlist_index = self.playlist.len() - 1;
        } else {
            self.playlist_index -= 1;
        }
        self.play_current()
    }

    /// Check if the current track has finished playing.
    pub fn is_finished(&mut self) -> bool {
        self.session
            .as_mut()
            .map(|session| session.is_finished())
            .unwrap_or_else(|| self.current_entry.is_some())
    }

    /// Auto-advance to next track if current finished.
    /// Returns true if a new track started.
    pub fn check_and_advance(&mut self) -> bool {
        if self.paused || self.current_entry.is_none() {
            return false;
        }
        if self.is_finished() {
            eprintln!(
                "local_player: track finished track={} advancing",
                current_track_label(self.current_entry.as_ref())
            );
            return self.next().is_ok();
        }
        false
    }

    /// Current playback position in milliseconds (estimated from wall clock).
    pub fn position_ms(&self) -> i64 {
        if self.current_entry.is_none() {
            return 0;
        }
        if let Some(position_ms) = self
            .session
            .as_ref()
            .and_then(|session| session.position_ms())
        {
            return position_ms;
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
        if let Some(session) = self.session.as_mut() {
            if self.paused {
                session.resume();
            }
            session.stop();
        }
        self.session = None;
    }
}

impl Drop for LocalPlayer {
    fn drop(&mut self) {
        self.stop_subprocess();
    }
}

fn local_playback_decoder_bin() -> &'static str {
    FFMPEG_TRANSCODER_BIN
}

fn playback_spawn_error(component: &str, kind: std::io::ErrorKind) -> LocalPlaybackError {
    match (component, kind) {
        ("ffmpeg", std::io::ErrorKind::NotFound) => LocalPlaybackError::MissingDecoder,
        _ => LocalPlaybackError::SpawnFailed,
    }
}

/// Spawn the SDL-backed playback session for a given audio file.
fn spawn_playback_session(file_path: &str) -> Result<PlaybackSession, LocalPlaybackError> {
    SdlPlayback::start(file_path).map(PlaybackSession::Sdl)
}

/// Spawn ffmpeg-lite as a PCM decoder. SDL is the only playback output.
fn spawn_ffmpeg_pcm(file_path: &str) -> Result<(Child, ChildStdout), LocalPlaybackError> {
    let decoder = local_playback_decoder_bin();
    eprintln!(
        "local_player: launching {} -> SDL PCM for {}",
        decoder, file_path
    );
    let mut ffmpeg = Command::new(decoder)
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
        .spawn()
        .map_err(|e| {
            eprintln!("local_player: ffmpeg spawn failed: {e}");
            playback_spawn_error("ffmpeg", e.kind())
        })?;
    eprintln!("local_player: ffmpeg started pid={}", ffmpeg.id());

    let ffmpeg_stdout = ffmpeg
        .stdout
        .take()
        .ok_or(LocalPlaybackError::MissingAudioPipe)?;

    Ok((ffmpeg, ffmpeg_stdout))
}

unsafe extern "C" fn sdl_audio_callback(userdata: *mut c_void, stream: *mut u8, len: c_int) {
    if userdata.is_null() || stream.is_null() || len <= 0 {
        return;
    }

    let state = &mut *(userdata as *mut SdlCallbackState);
    let output = std::slice::from_raw_parts_mut(stream as *mut i16, len as usize / 2);
    let read = match state.shared.ring.try_lock() {
        Ok(mut ring) => ring.read_samples(output),
        Err(_) => {
            output.fill(0);
            state.shared.underruns.fetch_add(1, Ordering::Relaxed);
            0
        }
    };
    if read < output.len() {
        state.shared.underruns.fetch_add(1, Ordering::Relaxed);
    }
    state
        .shared
        .output_samples
        .fetch_add(output.len() as u64, Ordering::Relaxed);
}

fn read_pcm_stdout(mut stdout: ChildStdout, shared: Arc<SdlShared>) {
    let mut bytes = [0u8; 8192];
    let mut pending_byte = None;
    let mut samples = Vec::with_capacity(bytes.len() / 2 + 1);

    loop {
        if shared.stop_requested.load(Ordering::Relaxed) {
            break;
        }

        let read = match stdout.read(&mut bytes) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) => {
                eprintln!("local_player: ffmpeg PCM read error: {e}");
                break;
            }
        };

        samples.clear();
        let mut idx = 0;
        if let Some(first) = pending_byte.take() {
            samples.push(i16::from_le_bytes([first, bytes[0]]));
            idx = 1;
        }
        while idx + 1 < read {
            samples.push(i16::from_le_bytes([bytes[idx], bytes[idx + 1]]));
            idx += 2;
        }
        if idx < read {
            pending_byte = Some(bytes[idx]);
        }

        write_samples_blocking(&shared, &samples);
    }

    shared.producer_eof.store(true, Ordering::Relaxed);
}

fn write_samples_blocking(shared: &Arc<SdlShared>, samples: &[i16]) {
    let mut offset = 0;
    while offset < samples.len() && !shared.stop_requested.load(Ordering::Relaxed) {
        let written = shared
            .ring
            .lock()
            .map(|mut ring| ring.write_samples(&samples[offset..]))
            .unwrap_or(0);
        offset += written;
        if written == 0 {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn current_track_label(entry: Option<&FavoriteEntry>) -> String {
    entry
        .map(|entry| format!("{} - {}", entry.artist, entry.name))
        .unwrap_or_else(|| "none".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::FFMPEG_TRANSCODER_BIN;

    #[test]
    fn pcm_ring_buffer_reads_silence_when_empty() {
        let mut ring = PcmRingBuffer::new(4);
        let mut out = [7i16; 6];

        let read = ring.read_samples(&mut out);

        assert_eq!(read, 0);
        assert_eq!(out, [0; 6]);
    }

    #[test]
    fn pcm_ring_buffer_wraps_without_reallocating() {
        let mut ring = PcmRingBuffer::new(4);

        assert_eq!(ring.write_samples(&[1, 2, 3]), 3);
        let mut first = [0i16; 2];
        assert_eq!(ring.read_samples(&mut first), 2);
        assert_eq!(first, [1, 2]);
        assert_eq!(ring.write_samples(&[4, 5, 6]), 3);

        let mut second = [0i16; 4];
        assert_eq!(ring.read_samples(&mut second), 4);
        assert_eq!(second, [3, 4, 5, 6]);
    }

    #[test]
    fn local_playback_decoder_defaults_to_bundled_ffmpeg_lite() {
        assert_eq!(local_playback_decoder_bin(), FFMPEG_TRANSCODER_BIN);
    }

    #[test]
    fn missing_decoder_error_uses_short_user_notice() {
        let err = playback_spawn_error("ffmpeg", std::io::ErrorKind::NotFound);
        assert_eq!(err.notice(), "Missing ffmpeg");
    }
}
