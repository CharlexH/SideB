use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::raw::{c_char, c_int, c_void};
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::constants::FFMPEG_TRANSCODER_BIN;
use crate::favorites::FavoriteEntry;

const PCM_SAMPLE_RATE: i32 = 48_000;
const PCM_CHANNELS: usize = 2;
const PCM_RING_SECONDS: usize = 3;
const DEFAULT_VOLUME_PERCENT: u32 = 80;
const MAX_VOLUME_PERCENT: u32 = 100;
const AUDIO_ROUTE_RETRY_SECONDS: u64 = 5;
const DEFAULT_SPOTIFY_PIPE_PATH: &str = "/tmp/sideb-spotify.pcm";
const SPOTIFY_PIPE_PATH_ENV: &str = "SIDEB_SPOTIFY_PIPE";
const SPOTIFY_PIPE_INPUT_SAMPLE_RATE_ENV: &str = "SIDEB_SPOTIFY_PIPE_SAMPLE_RATE";
const DEFAULT_SPOTIFY_PIPE_INPUT_SAMPLE_RATE: usize = 44_100;
const SPOTIFY_ROUTE_CHECK_INTERVAL: Duration = Duration::from_millis(500);
const SPOTIFY_PIPE_IDLE_CLOSE: Duration = Duration::from_millis(1500);

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

    fn set_volume_percent(&self, percent: u32) {
        match self {
            Self::Sdl(session) => session.set_volume_percent(percent),
        }
    }

    fn audio_device(&self) -> Option<&str> {
        match self {
            Self::Sdl(session) => session.audio_device(),
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
    volume_percent: AtomicU32,
}

impl SdlShared {
    fn new(volume_percent: u32) -> Self {
        Self {
            ring: Mutex::new(PcmRingBuffer::new(
                PCM_SAMPLE_RATE as usize * PCM_CHANNELS * PCM_RING_SECONDS,
            )),
            producer_eof: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            output_samples: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            volume_percent: AtomicU32::new(clamp_volume_percent(volume_percent)),
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
        label: &str,
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
        let mut have = empty_sdl_audio_spec();
        let id = unsafe { (lib.open_audio_device)(std::ptr::null(), 0, &want, &mut have, 0) };
        if id == 0 {
            eprintln!(
                "local_player: SDL open audio failed device={}: {}",
                label,
                lib.error_string()
            );
            unsafe { (lib.quit_sub_system)(SDL_INIT_AUDIO) };
            return Err(LocalPlaybackError::SpawnFailed);
        }

        eprintln!(
            "local_player: SDL audio opened device={} freq={} channels={} samples={}",
            label, have.freq, have.channels, have.samples
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
    start_position_ms: i64,
    audio_device: Option<String>,
}

impl SdlPlayback {
    fn start(
        file_path: &str,
        volume_percent: u32,
        start_position_ms: i64,
        audio_device: Option<&str>,
    ) -> Result<Self, LocalPlaybackError> {
        set_sdl_audio_device_env(audio_device);
        let label = audio_device.unwrap_or("SDL default");
        let lib = SdlLibrary::load()?;
        let shared = Arc::new(SdlShared::new(volume_percent));
        let (device, callback_state, obtained) =
            SdlAudioDevice::open(Arc::clone(&lib), Arc::clone(&shared), label)?;
        let (ffmpeg_child, stdout) = spawn_ffmpeg_pcm(file_path, start_position_ms)?;
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
            start_position_ms: normalize_start_position_ms(start_position_ms),
            audio_device: audio_device.map(ToOwned::to_owned),
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
        self.start_position_ms + ((frames * 1000) / self.sample_rate.max(1) as u64) as i64
    }

    fn set_volume_percent(&self, percent: u32) {
        self.shared
            .volume_percent
            .store(clamp_volume_percent(percent), Ordering::Relaxed);
    }

    fn audio_device(&self) -> Option<&str> {
        self.audio_device.as_deref()
    }
}

impl Drop for SdlPlayback {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct SpotifyPipeAudio {
    volume_percent: Arc<AtomicU32>,
    suspended: Arc<AtomicBool>,
}

impl SpotifyPipeAudio {
    pub fn start(quit: Arc<AtomicBool>, initial_volume_percent: u32) -> Self {
        let pipe_path = spotify_pipe_path();
        let input_sample_rate = spotify_pipe_input_sample_rate();
        let volume_percent = Arc::new(AtomicU32::new(clamp_volume_percent(initial_volume_percent)));
        let suspended = Arc::new(AtomicBool::new(false));

        let thread_volume = Arc::clone(&volume_percent);
        let thread_suspended = Arc::clone(&suspended);
        let spawn_result = thread::Builder::new()
            .name("spotify-audio".into())
            .spawn(move || {
                run_spotify_pipe_audio(
                    pipe_path,
                    input_sample_rate,
                    quit,
                    thread_volume,
                    thread_suspended,
                );
            });

        if let Err(error) = spawn_result {
            eprintln!("spotify_audio: thread spawn failed: {error}");
        }

        Self {
            volume_percent,
            suspended,
        }
    }

    pub fn set_volume_percent(&self, percent: u32) {
        self.volume_percent
            .store(clamp_volume_percent(percent), Ordering::Relaxed);
    }

    pub fn suspend(&self) {
        if !self.suspended.swap(true, Ordering::Relaxed) {
            eprintln!("spotify_audio: suspended");
        }
    }

    pub fn resume(&self) {
        if self.suspended.swap(false, Ordering::Relaxed) {
            eprintln!("spotify_audio: resumed");
        }
    }
}

struct SpotifySdlOutput {
    shared: Arc<SdlShared>,
    lib: Option<Arc<SdlLibrary>>,
    device: Option<SdlAudioDevice>,
    callback_state: Option<Box<SdlCallbackState>>,
    audio_device: Option<String>,
    last_route_check: Option<Instant>,
}

impl SpotifySdlOutput {
    fn new(shared: Arc<SdlShared>) -> Self {
        Self {
            shared,
            lib: None,
            device: None,
            callback_state: None,
            audio_device: None,
            last_route_check: None,
        }
    }

    fn ensure_route(&mut self) -> bool {
        let now = Instant::now();
        if self.device.is_some()
            && self
                .last_route_check
                .map(|last| now.duration_since(last) < SPOTIFY_ROUTE_CHECK_INTERVAL)
                .unwrap_or(false)
        {
            return true;
        }
        self.last_route_check = Some(now);

        let desired_audio_device = preferred_audio_device();
        if self.device.is_some() && desired_audio_device == self.audio_device {
            return true;
        }

        if self.device.is_some() {
            eprintln!(
                "spotify_audio: audio route changed from={} to={} reopening SDL",
                self.audio_device.as_deref().unwrap_or("default"),
                desired_audio_device.as_deref().unwrap_or("default")
            );
            self.close_device();
        }

        match self.open_device(desired_audio_device.as_deref()) {
            Ok(()) => {
                self.audio_device = desired_audio_device;
                true
            }
            Err(first_error) if desired_audio_device.is_some() => {
                eprintln!(
                    "spotify_audio: SDL routed audio unavailable device={} error={first_error:?}",
                    desired_audio_device.as_deref().unwrap_or("unknown")
                );
                match self.open_device(None) {
                    Ok(()) => {
                        self.audio_device = None;
                        true
                    }
                    Err(error) => {
                        eprintln!("spotify_audio: SDL default audio unavailable error={error:?}");
                        false
                    }
                }
            }
            Err(error) => {
                eprintln!("spotify_audio: SDL audio unavailable error={error:?}");
                false
            }
        }
    }

    fn close_for_idle(&mut self, last_audio_at: Instant) {
        if self.device.is_some() && last_audio_at.elapsed() >= SPOTIFY_PIPE_IDLE_CLOSE {
            eprintln!("spotify_audio: closing idle SDL output");
            self.close_device();
        }
    }

    fn close_device(&mut self) {
        if let Some(device) = self.device.take() {
            device.pause(true);
            drop(device);
        }
        self.callback_state = None;
        self.audio_device = None;
        if let Ok(mut ring) = self.shared.ring.lock() {
            ring.clear();
        }
    }

    fn open_device(&mut self, audio_device: Option<&str>) -> Result<(), LocalPlaybackError> {
        set_sdl_audio_device_env(audio_device);
        let label = audio_device.unwrap_or("SDL default");
        let lib = match self.lib.as_ref() {
            Some(lib) => Arc::clone(lib),
            None => {
                let lib = SdlLibrary::load()?;
                self.lib = Some(Arc::clone(&lib));
                lib
            }
        };
        let (device, callback_state, obtained) =
            SdlAudioDevice::open(lib, Arc::clone(&self.shared), label)?;
        device.pause(false);
        self.callback_state = Some(callback_state);
        self.device = Some(device);
        eprintln!(
            "spotify_audio: SDL output ready device={} freq={} channels={}",
            label, obtained.freq, obtained.channels
        );
        Ok(())
    }
}

impl Drop for SpotifySdlOutput {
    fn drop(&mut self) {
        self.close_device();
    }
}

#[derive(Debug)]
struct StereoS16Resampler {
    input_rate: usize,
    output_rate: usize,
    position: f64,
    buffered_samples: Vec<i16>,
}

impl StereoS16Resampler {
    fn new(input_rate: usize, output_rate: usize) -> Self {
        Self {
            input_rate: input_rate.max(1),
            output_rate: output_rate.max(1),
            position: 0.0,
            buffered_samples: Vec::new(),
        }
    }

    fn resample(&mut self, input: &[i16]) -> Vec<i16> {
        if input.is_empty() {
            return Vec::new();
        }
        if self.input_rate == self.output_rate {
            return input.to_vec();
        }

        self.buffered_samples.extend_from_slice(input);
        let frame_count = self.buffered_samples.len() / PCM_CHANNELS;
        if frame_count == 0 {
            return Vec::new();
        }

        let step = self.input_rate as f64 / self.output_rate as f64;
        let output_frames = ((frame_count as f64 - self.position).max(0.0) / step).ceil() as usize;
        let mut output = Vec::with_capacity(output_frames * PCM_CHANNELS);

        while self.position + 1e-9 < frame_count as f64 {
            let left_idx = self.position.floor() as usize;
            let right_idx = (left_idx + 1).min(frame_count - 1);
            let frac = self.position - left_idx as f64;
            for channel in 0..PCM_CHANNELS {
                let a = self.buffered_samples[left_idx * PCM_CHANNELS + channel] as f64;
                let b = self.buffered_samples[right_idx * PCM_CHANNELS + channel] as f64;
                output.push(
                    (a + (b - a) * frac)
                        .round()
                        .clamp(i16::MIN as f64, i16::MAX as f64) as i16,
                );
            }
            self.position += step;
        }

        let consumed_frames = self.position.floor().min(frame_count as f64) as usize;
        if consumed_frames > 0 {
            let consumed_samples = consumed_frames * PCM_CHANNELS;
            self.buffered_samples.drain(0..consumed_samples);
            self.position -= consumed_frames as f64;
        }

        output
    }
}

fn run_spotify_pipe_audio(
    pipe_path: String,
    input_sample_rate: usize,
    quit: Arc<AtomicBool>,
    volume_percent: Arc<AtomicU32>,
    suspended: Arc<AtomicBool>,
) {
    eprintln!(
        "spotify_audio: starting pipe reader path={} input_rate={} output_rate={}",
        pipe_path, input_sample_rate, PCM_SAMPLE_RATE
    );

    let shared = Arc::new(SdlShared::new(volume_percent.load(Ordering::Relaxed)));
    let mut output = SpotifySdlOutput::new(Arc::clone(&shared));
    let mut pipe = None;
    let mut bytes = [0u8; 8192];
    let mut pending_byte = None;
    let mut samples = Vec::with_capacity(bytes.len() / 2 + 1);
    let mut resampler = StereoS16Resampler::new(input_sample_rate, PCM_SAMPLE_RATE as usize);
    let mut last_audio_at = Instant::now();

    while !quit.load(Ordering::Relaxed) {
        shared
            .volume_percent
            .store(volume_percent.load(Ordering::Relaxed), Ordering::Relaxed);

        if pipe.is_none() {
            match open_spotify_pipe_reader(&pipe_path) {
                Ok(reader) => {
                    eprintln!("spotify_audio: pipe reader opened path={pipe_path}");
                    pipe = Some(reader);
                }
                Err(error) => {
                    eprintln!("spotify_audio: pipe open failed path={pipe_path} error={error}");
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            }
        }

        let read_result = pipe.as_mut().unwrap().read(&mut bytes);
        let read = match read_result {
            Ok(0) => {
                output.close_for_idle(last_audio_at);
                thread::sleep(Duration::from_millis(40));
                continue;
            }
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                output.close_for_idle(last_audio_at);
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                eprintln!("spotify_audio: pipe read error: {error}");
                pipe = None;
                output.close_device();
                thread::sleep(Duration::from_millis(250));
                continue;
            }
        };

        last_audio_at = Instant::now();
        samples.clear();
        append_s16le_samples(&bytes[..read], &mut pending_byte, &mut samples);
        let output_samples = resampler.resample(&samples);
        if output_samples.is_empty() {
            continue;
        }

        if suspended.load(Ordering::Relaxed) {
            output.close_device();
            continue;
        }

        if output.ensure_route() {
            let _all_written = write_samples_realtime(&shared, &output_samples, || {
                if suspended.load(Ordering::Relaxed) {
                    output.close_device();
                    false
                } else {
                    output.ensure_route()
                }
            });
        }
    }

    shared.stop_requested.store(true, Ordering::Relaxed);
    output.close_device();
    eprintln!("spotify_audio: stopped");
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
    volume_percent: u32,
    base_position_ms: i64,
    last_audio_route_retry: Option<Instant>,
    waiting_for_audio_route: bool,
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
            volume_percent: DEFAULT_VOLUME_PERCENT,
            base_position_ms: 0,
            last_audio_route_retry: None,
            waiting_for_audio_route: false,
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
        self.play_current_from(0)
    }

    fn play_current_from(&mut self, start_position_ms: i64) -> Result<(), LocalPlaybackError> {
        let start_position_ms = normalize_start_position_ms(start_position_ms);
        self.stop_subprocess();
        self.current_entry = None;
        self.waiting_for_audio_route = false;

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

            match spawn_playback_session(&file_path, self.volume_percent, start_position_ms) {
                Ok(session) => {
                    self.session = Some(session);
                    self.current_entry = Some(entry);
                    self.playlist_index = idx;
                    self.start_time = Instant::now();
                    self.paused = false;
                    self.paused_elapsed = Duration::ZERO;
                    self.base_position_ms = start_position_ms;
                    self.waiting_for_audio_route = false;
                    eprintln!(
                        "local_player: pipeline ready uri={} backend={} volume_percent={} start_ms={}",
                        self.current_entry
                            .as_ref()
                            .map(|entry| entry.uri.as_str())
                            .unwrap_or("unknown"),
                        self.session
                            .as_ref()
                            .map(|session| session.label())
                            .unwrap_or("none"),
                        self.volume_percent,
                        start_position_ms
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
        self.base_position_ms = 0;
        self.waiting_for_audio_route = false;
        self.playlist.clear();
        self.playlist_index = 0;
        eprintln!("local_player: stopped track={stopped_track}");
    }

    pub fn set_volume_percent(&mut self, percent: u32) {
        let percent = clamp_volume_percent(percent);
        if self.volume_percent == percent {
            return;
        }
        self.volume_percent = percent;
        if let Some(session) = self.session.as_ref() {
            session.set_volume_percent(percent);
        }
        eprintln!("local_player: volume percent={percent}");
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
        if self.waiting_for_audio_route {
            return false;
        }
        self.session
            .as_mut()
            .map(|session| session.is_finished())
            .unwrap_or_else(|| self.current_entry.is_some())
    }

    /// Auto-advance to next track if current finished.
    /// Returns true if a new track started.
    pub fn check_and_advance(&mut self) -> bool {
        if self.paused || self.waiting_for_audio_route || self.current_entry.is_none() {
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

    pub fn refresh_audio_route(&mut self) -> bool {
        if self.paused || self.current_entry.is_none() {
            return false;
        }

        let desired_audio_device = preferred_audio_device();
        let current_audio_device = self
            .session
            .as_ref()
            .and_then(|session| session.audio_device())
            .map(ToOwned::to_owned);
        if desired_audio_device == current_audio_device && !self.waiting_for_audio_route {
            return false;
        }

        let now = Instant::now();
        if self
            .last_audio_route_retry
            .map(|last| now.duration_since(last) < Duration::from_secs(AUDIO_ROUTE_RETRY_SECONDS))
            .unwrap_or(false)
        {
            return false;
        }
        self.last_audio_route_retry = Some(now);

        let resume_ms = self.position_ms();
        eprintln!(
            "local_player: audio route changed from={} to={} restarting current track resume_ms={}",
            current_audio_device.as_deref().unwrap_or("default"),
            desired_audio_device.as_deref().unwrap_or("default"),
            resume_ms
        );
        match self.restart_current_for_audio_route(resume_ms) {
            Ok(()) => true,
            Err(error) => {
                eprintln!("local_player: audio route restart failed error={error:?}");
                false
            }
        }
    }

    /// Current playback position in milliseconds (estimated from wall clock).
    pub fn position_ms(&self) -> i64 {
        if self.current_entry.is_none() {
            return 0;
        }
        if self.waiting_for_audio_route {
            return self.base_position_ms;
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
        self.base_position_ms + elapsed.as_millis() as i64
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

    fn restart_current_for_audio_route(
        &mut self,
        start_position_ms: i64,
    ) -> Result<(), LocalPlaybackError> {
        let start_position_ms = normalize_start_position_ms(start_position_ms);
        let idx = self.playlist_index;
        let entry = self
            .playlist
            .get(idx)
            .cloned()
            .or_else(|| self.current_entry.clone())
            .ok_or(LocalPlaybackError::NoPlayableTrack)?;

        let Some(file_path) = entry.file_path.clone() else {
            self.waiting_for_audio_route = false;
            return Err(LocalPlaybackError::FileMissing);
        };
        if !std::path::Path::new(&file_path).exists() {
            self.waiting_for_audio_route = false;
            return Err(LocalPlaybackError::FileMissing);
        }

        self.stop_subprocess();
        self.current_entry = Some(entry.clone());
        self.playlist_index = idx;
        self.base_position_ms = start_position_ms;
        self.start_time = Instant::now();
        self.paused_elapsed = Duration::ZERO;

        eprintln!(
            "local_player: route restart idx={}/{} uri={} track={} - {} path={}",
            idx + 1,
            self.playlist.len(),
            entry.uri,
            entry.artist,
            entry.name,
            file_path
        );

        match spawn_playback_session(&file_path, self.volume_percent, start_position_ms) {
            Ok(session) => {
                self.session = Some(session);
                self.current_entry = Some(entry);
                self.paused = false;
                self.waiting_for_audio_route = false;
                eprintln!(
                    "local_player: pipeline ready uri={} backend={} volume_percent={} start_ms={}",
                    self.current_entry
                        .as_ref()
                        .map(|entry| entry.uri.as_str())
                        .unwrap_or("unknown"),
                    self.session
                        .as_ref()
                        .map(|session| session.label())
                        .unwrap_or("none"),
                    self.volume_percent,
                    start_position_ms
                );
                Ok(())
            }
            Err(error) => {
                self.session = None;
                self.current_entry = Some(entry);
                self.waiting_for_audio_route = error == LocalPlaybackError::SpawnFailed;
                eprintln!(
                    "local_player: audio route pending track={} resume_ms={} error={error:?}",
                    current_track_label(self.current_entry.as_ref()),
                    start_position_ms
                );
                Err(error)
            }
        }
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

fn default_spotify_pipe_path() -> &'static str {
    DEFAULT_SPOTIFY_PIPE_PATH
}

fn spotify_pipe_path() -> String {
    std::env::var(SPOTIFY_PIPE_PATH_ENV).unwrap_or_else(|_| default_spotify_pipe_path().to_string())
}

fn spotify_pipe_input_sample_rate() -> usize {
    std::env::var(SPOTIFY_PIPE_INPUT_SAMPLE_RATE_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|rate| *rate > 0)
        .unwrap_or(DEFAULT_SPOTIFY_PIPE_INPUT_SAMPLE_RATE)
}

fn playback_spawn_error(component: &str, kind: std::io::ErrorKind) -> LocalPlaybackError {
    match (component, kind) {
        ("ffmpeg", std::io::ErrorKind::NotFound) => LocalPlaybackError::MissingDecoder,
        _ => LocalPlaybackError::SpawnFailed,
    }
}

fn empty_sdl_audio_spec() -> SdlAudioSpec {
    SdlAudioSpec {
        freq: 0,
        format: 0,
        channels: 0,
        silence: 0,
        samples: 0,
        padding: 0,
        size: 0,
        callback: None,
        userdata: std::ptr::null_mut(),
    }
}

fn sdl_audio_targets() -> Vec<SdlAudioTarget> {
    let cards = std::fs::read_to_string("/proc/asound/cards").unwrap_or_default();
    sdl_audio_targets_from_cards(&cards)
}

fn sdl_audio_targets_from_cards(cards: &str) -> Vec<SdlAudioTarget> {
    let mut external = Vec::new();
    let mut internal = Vec::new();

    for card in parse_alsa_cards(cards) {
        let external_card = !card.id.eq_ignore_ascii_case("audiocodec");
        let target = SdlAudioTarget {
            device: format!("plughw:{},0", card.id),
            external: external_card,
        };
        if external_card {
            push_unique(&mut external, target);
        } else {
            push_unique(&mut internal, target);
        }
    }

    let mut targets = external;
    targets.extend(internal);
    targets
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SdlAudioTarget {
    device: String,
    external: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AlsaCard {
    id: String,
}

fn parse_alsa_cards(cards: &str) -> Vec<AlsaCard> {
    cards
        .lines()
        .filter_map(|line| {
            let start = line.find('[')?;
            let rest = &line[start + 1..];
            let end = rest.find(']')?;
            let id = rest[..end].trim();
            if id.is_empty() {
                None
            } else {
                Some(AlsaCard { id: id.to_string() })
            }
        })
        .collect()
}

fn push_unique<T: PartialEq>(targets: &mut Vec<T>, target: T) {
    if !targets.contains(&target) {
        targets.push(target);
    }
}

fn preferred_audio_device() -> Option<String> {
    keep_usb_audio_power_awake();
    let cards = std::fs::read_to_string("/proc/asound/cards").unwrap_or_default();
    preferred_audio_device_from_cards(&cards)
}

fn keep_usb_audio_power_awake() {
    set_usb_host_controller_power_awake();
    set_usb_device_power_awake();
}

fn set_usb_host_controller_power_awake() {
    let Ok(entries) = std::fs::read_dir("/sys/devices/platform/soc") else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if (name.contains("ehci") || name.contains("ohci")) && name.contains("controller") {
            let _ = std::fs::write(entry.path().join("power/control"), "on\n");
        }
    }
}

fn set_usb_device_power_awake() {
    let Ok(entries) = std::fs::read_dir("/sys/bus/usb/devices") else {
        return;
    };
    for entry in entries.flatten() {
        let _ = std::fs::write(entry.path().join("power/control"), "on\n");
    }
}

fn preferred_audio_device_from_cards(cards: &str) -> Option<String> {
    sdl_audio_targets_from_cards(cards)
        .into_iter()
        .next()
        .map(|target| target.device)
}

fn set_sdl_audio_device_env(audio_device: Option<&str>) {
    match audio_device {
        Some(device) => std::env::set_var("AUDIODEV", device),
        None => std::env::remove_var("AUDIODEV"),
    }
}

fn normalize_start_position_ms(position_ms: i64) -> i64 {
    position_ms.max(0)
}

fn ffmpeg_seek_arg(start_position_ms: i64) -> Option<String> {
    let start_position_ms = normalize_start_position_ms(start_position_ms);
    (start_position_ms > 0).then(|| format!("{:.3}", start_position_ms as f64 / 1000.0))
}

pub fn local_volume_percent(volume: i32, volume_max: i32) -> u32 {
    if volume_max <= 0 {
        return DEFAULT_VOLUME_PERCENT;
    }
    let volume = volume.clamp(0, volume_max) as i64;
    let volume_max = volume_max as i64;
    ((volume * MAX_VOLUME_PERCENT as i64 + volume_max / 2) / volume_max)
        .clamp(0, MAX_VOLUME_PERCENT as i64) as u32
}

fn clamp_volume_percent(percent: u32) -> u32 {
    percent.min(MAX_VOLUME_PERCENT)
}

fn scale_pcm_sample(sample: i16, volume_percent: u32) -> i16 {
    let scaled = sample as i32 * clamp_volume_percent(volume_percent) as i32 / 100;
    scaled.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

fn apply_volume_to_samples(samples: &mut [i16], volume_percent: u32) {
    let volume_percent = clamp_volume_percent(volume_percent);
    if volume_percent == MAX_VOLUME_PERCENT {
        return;
    }
    for sample in samples {
        *sample = scale_pcm_sample(*sample, volume_percent);
    }
}

/// Spawn the best available playback session for a given audio file.
fn spawn_playback_session(
    file_path: &str,
    volume_percent: u32,
    start_position_ms: i64,
) -> Result<PlaybackSession, LocalPlaybackError> {
    let audio_device = preferred_audio_device();
    match SdlPlayback::start(
        file_path,
        volume_percent,
        start_position_ms,
        audio_device.as_deref(),
    ) {
        Ok(session) => Ok(PlaybackSession::Sdl(session)),
        Err(first_error) if audio_device.is_some() => {
            eprintln!(
                "local_player: SDL routed audio unavailable device={} error={first_error:?}",
                audio_device.as_deref().unwrap_or("unknown")
            );
            SdlPlayback::start(file_path, volume_percent, start_position_ms, None)
                .map(PlaybackSession::Sdl)
        }
        Err(error) => Err(error),
    }
}

/// Spawn ffmpeg-lite as the controlled PCM decoder for local playback.
fn spawn_ffmpeg_pcm(
    file_path: &str,
    start_position_ms: i64,
) -> Result<(Child, ChildStdout), LocalPlaybackError> {
    let decoder = local_playback_decoder_bin();
    eprintln!(
        "local_player: launching {} -> playback PCM for {} start_ms={}",
        decoder,
        file_path,
        normalize_start_position_ms(start_position_ms)
    );
    let sample_rate = PCM_SAMPLE_RATE.to_string();
    let seek_arg = ffmpeg_seek_arg(start_position_ms);
    let mut command = Command::new(decoder);
    if let Some(seek_arg) = seek_arg.as_deref() {
        command.args(["-ss", seek_arg]);
    }
    let mut ffmpeg = command
        .args(["-i", file_path, "-f", "s16le", "-ar"])
        .arg(sample_rate)
        .args(["-ac", "2", "-loglevel", "quiet", "pipe:1"])
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
    let volume = state.shared.volume_percent.load(Ordering::Relaxed);
    apply_volume_to_samples(output, volume);
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
        append_s16le_samples(&bytes[..read], &mut pending_byte, &mut samples);

        write_samples_blocking(&shared, &samples);
    }

    shared.producer_eof.store(true, Ordering::Relaxed);
}

fn append_s16le_samples(bytes: &[u8], pending_byte: &mut Option<u8>, samples: &mut Vec<i16>) {
    let mut idx = 0;
    if let Some(first) = pending_byte.take() {
        if let Some(second) = bytes.first() {
            samples.push(i16::from_le_bytes([first, *second]));
            idx = 1;
        } else {
            *pending_byte = Some(first);
            return;
        }
    }
    while idx + 1 < bytes.len() {
        samples.push(i16::from_le_bytes([bytes[idx], bytes[idx + 1]]));
        idx += 2;
    }
    if idx < bytes.len() {
        *pending_byte = Some(bytes[idx]);
    }
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

fn write_samples_realtime<F>(shared: &Arc<SdlShared>, samples: &[i16], mut route_ready: F) -> bool
where
    F: FnMut() -> bool,
{
    let mut offset = 0;
    while offset < samples.len() && !shared.stop_requested.load(Ordering::Relaxed) {
        if !route_ready() {
            return false;
        }
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
    offset == samples.len()
}

fn write_samples_lossy(shared: &Arc<SdlShared>, samples: &[i16]) {
    let mut offset = 0;
    while offset < samples.len() && !shared.stop_requested.load(Ordering::Relaxed) {
        let written = shared
            .ring
            .lock()
            .map(|mut ring| ring.write_samples(&samples[offset..]))
            .unwrap_or(0);
        if written == 0 {
            break;
        }
        offset += written;
    }
}

fn open_spotify_pipe_reader(path: &str) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
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
    use crate::favorites::FavoriteSource;

    fn test_entry(uri: &str) -> FavoriteEntry {
        FavoriteEntry {
            uri: uri.to_string(),
            name: "Track".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            cover_url: String::new(),
            source: FavoriteSource::Spotify,
            file_path: Some(format!("/tmp/{uri}.mp3")),
            cover_path: None,
            duration_ms: Some(1_000),
            spotify_duration_ms: None,
            downloaded: true,
            added_at: "0".to_string(),
        }
    }

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
    fn spotify_pipe_realtime_write_waits_for_ring_space_instead_of_dropping() {
        let shared = Arc::new(SdlShared {
            ring: Mutex::new(PcmRingBuffer::new(2)),
            producer_eof: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            output_samples: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            volume_percent: AtomicU32::new(DEFAULT_VOLUME_PERCENT),
        });
        shared.ring.lock().unwrap().write_samples(&[1, 2]);

        let consumer_shared = Arc::clone(&shared);
        let consumer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            let mut drained = [0i16; 2];
            consumer_shared
                .ring
                .lock()
                .unwrap()
                .read_samples(&mut drained);
            drained
        });

        assert!(write_samples_realtime(&shared, &[3, 4], || true));
        assert_eq!(consumer.join().unwrap(), [1, 2]);

        let mut remaining = [0i16; 2];
        shared.ring.lock().unwrap().read_samples(&mut remaining);
        assert_eq!(remaining, [3, 4]);
    }

    #[test]
    fn local_playback_decoder_defaults_to_bundled_ffmpeg_lite() {
        assert_eq!(local_playback_decoder_bin(), FFMPEG_TRANSCODER_BIN);
    }

    #[test]
    fn local_playback_pcm_matches_trimui_audio_device_rate() {
        assert_eq!(PCM_SAMPLE_RATE, 48_000);
    }

    #[test]
    fn spotify_pipe_path_defaults_to_tmp_fifo() {
        assert_eq!(default_spotify_pipe_path(), "/tmp/sideb-spotify.pcm");
    }

    #[test]
    fn spotify_pipe_resampler_expands_44100_stereo_to_48000() {
        let input_frames = 441usize;
        let mut input = Vec::with_capacity(input_frames * 2);
        for frame in 0..input_frames {
            input.push(frame as i16);
            input.push(-(frame as i16));
        }

        let mut resampler = StereoS16Resampler::new(44_100, 48_000);
        let output = resampler.resample(&input);

        assert_eq!(output.len(), 480 * 2);
        assert_eq!(&output[..2], &[0, 0]);
        assert!(output[2] >= 0);
        assert!(output[3] <= 0);
    }

    #[test]
    fn local_volume_percent_inherits_app_volume_scale() {
        assert_eq!(local_volume_percent(80, 100), 80);
        assert_eq!(local_volume_percent(32, 64), 50);
        assert_eq!(local_volume_percent(999, 100), 100);
        assert_eq!(local_volume_percent(-5, 100), 0);
        assert_eq!(local_volume_percent(25, 0), DEFAULT_VOLUME_PERCENT);
    }

    #[test]
    fn ffmpeg_seek_arg_formats_resume_position() {
        assert_eq!(ffmpeg_seek_arg(0), None);
        assert_eq!(ffmpeg_seek_arg(-400), None);
        assert_eq!(ffmpeg_seek_arg(12_345), Some("12.345".to_string()));
        assert_eq!(ffmpeg_seek_arg(12_300), Some("12.300".to_string()));
    }

    #[test]
    fn pcm_volume_scaling_applies_percent_without_clipping() {
        assert_eq!(scale_pcm_sample(10_000, 50), 5_000);
        assert_eq!(scale_pcm_sample(-10_000, 50), -5_000);
        assert_eq!(scale_pcm_sample(12_345, 100), 12_345);
        assert_eq!(scale_pcm_sample(12_345, 150), 12_345);

        let mut samples = [10_000, -10_000, i16::MAX, i16::MIN];
        apply_volume_to_samples(&mut samples, 25);
        assert_eq!(samples, [2_500, -2_500, 8_191, -8_192]);
    }

    #[test]
    fn sdl_audio_targets_prefer_usb_audiodev_over_internal() {
        let cards = "\
 0 [audiocodec     ]: audiocodec - audiocodec
                      audiocodec
 1 [Plus           ]: USB-Audio - JAM Plus
                      Apogee Electronics JAM Plus at usb-sunxi-ohci-1, full speed
";

        let targets = sdl_audio_targets_from_cards(cards);

        assert_eq!(
            targets,
            vec![
                SdlAudioTarget {
                    device: "plughw:Plus,0".to_string(),
                    external: true,
                },
                SdlAudioTarget {
                    device: "plughw:audiocodec,0".to_string(),
                    external: false,
                },
            ]
        );
    }

    #[test]
    fn preferred_audio_device_falls_back_to_internal_card() {
        let cards = "\
 0 [audiocodec     ]: audiocodec - audiocodec
                      audiocodec
";

        assert_eq!(
            preferred_audio_device_from_cards(cards),
            Some("plughw:audiocodec,0".to_string())
        );
    }

    #[test]
    fn waiting_for_audio_route_does_not_advance_playlist() {
        let first = test_entry("track:1");
        let second = test_entry("track:2");
        let mut player = LocalPlayer::new();
        player.playlist = vec![first.clone(), second];
        player.playlist_index = 0;
        player.current_entry = Some(first);
        player.base_position_ms = 8_362;
        player.waiting_for_audio_route = true;

        assert!(!player.check_and_advance());
        assert!(!player.is_finished());
        assert_eq!(player.playlist_index, 0);
        assert_eq!(player.position_ms(), 8_362);
        assert_eq!(
            player.current_entry().map(|entry| entry.uri.as_str()),
            Some("track:1")
        );
    }

    #[test]
    fn missing_decoder_error_uses_short_user_notice() {
        let err = playback_spawn_error("ffmpeg", std::io::ErrorKind::NotFound);
        assert_eq!(err.notice(), "Missing ffmpeg");
    }
}
