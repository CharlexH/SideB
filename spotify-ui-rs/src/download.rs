use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::constants::{FFMPEG_TRANSCODER_BIN, NODE_BIN, YTDLP_BIN};
use crate::favorites::FavoriteEntry;
use crate::favorites::FavoritesManager;
use crate::log_utils::{exit_status_label, format_bytes, summarize_command_output};
use crate::mode::AppMode;
use crate::network::is_allowed_cover_url;
use crate::paths::app_paths;

const MAX_RETRIES: u32 = 1;
const RETRY_DELAY_SECS: u64 = 3;
const CANDIDATE_COUNT: usize = 5;
const DURATION_REJECT_THRESHOLD_MS: i64 = 15_000;
const PROGRESS_THROTTLE_MS: u128 = 400;
const MAX_CAPTURED_STDERR_BYTES: usize = 16 * 1024;
const MAX_CAPTURED_STDOUT_BYTES: usize = 512 * 1024;
const YTDLP_SEARCH_TIMEOUT: Duration = Duration::from_secs(60);
const YTDLP_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const YTDLP_TEMP_SUBDIR: &str = "tmp/yt-dlp";

/// Download phase visible to the UI.
/// Overall progress: Queued=0%, Searching=0-25%, Downloading=25-75%, Transcoding=75-100%.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DownloadPhase {
    /// Waiting in queue for previous downloads to finish.
    Queued,
    /// Searching YouTube for candidates (0.0 .. 1.0 within this phase).
    Searching,
    /// Downloading audio from YouTube (0.0 .. 1.0 within this phase).
    Downloading(f32),
    /// Post-download transcoding to mp3.
    Transcoding,
}

impl DownloadPhase {
    /// Map phase to overall 0.0..1.0 progress for the pie indicator.
    pub fn overall_progress(&self) -> f32 {
        match self {
            Self::Queued => 0.0,
            Self::Searching => 0.125, // midpoint of 0%-25%
            Self::Downloading(pct) => 0.25 + pct * 0.50,
            Self::Transcoding => 0.875, // midpoint of 75%-100%
        }
    }
}

/// Shared progress map: URI → current phase. Entries are removed on completion.
pub type DownloadProgressMap = Arc<Mutex<HashMap<String, DownloadPhase>>>;

/// A request to download a track in the background.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadRequest {
    pub uri: String,
    pub track_name: String,
    pub artist_name: String,
    pub cover_url: String,
    pub spotify_duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownloadFailureKind {
    CookiesBotCheck,
    SignatureChallenge,
    Network,
    TempStorage,
    MissingYtDlp,
    MissingTranscoder,
    NoMatchingAudio,
    Generic,
}

impl DownloadFailureKind {
    fn notice(self) -> &'static str {
        match self {
            Self::CookiesBotCheck => "Cookie check failed",
            Self::SignatureChallenge => "YouTube challenge",
            Self::Network => "Network error",
            Self::TempStorage => "Storage full",
            Self::MissingYtDlp => "Missing yt-dlp",
            Self::MissingTranscoder => "Audio tool failed",
            Self::NoMatchingAudio => "No audio match",
            Self::Generic => "Download failed",
        }
    }

    fn priority(self) -> u8 {
        match self {
            Self::Generic => 0,
            Self::NoMatchingAudio => 1,
            Self::Network => 2,
            Self::CookiesBotCheck => 3,
            Self::SignatureChallenge => 4,
            Self::TempStorage => 5,
            Self::MissingTranscoder => 6,
            Self::MissingYtDlp => 7,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownloadOutcome {
    Success,
    Skipped,
    Failed(DownloadFailureKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingQueueAction {
    Remove,
    KeepForRetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuePlacement {
    Back,
    Front,
}

/// A candidate result from YouTube search metadata.
struct SearchCandidate {
    id: String,
    title: String,
    duration_secs: Option<f64>,
    channel: Option<String>,
}

#[derive(Clone)]
struct DownloadWorkQueue {
    inner: Arc<(Mutex<VecDeque<DownloadRequest>>, Condvar)>,
}

impl DownloadWorkQueue {
    fn new() -> Self {
        Self {
            inner: Arc::new((Mutex::new(VecDeque::new()), Condvar::new())),
        }
    }

    fn enqueue_back(&self, request: DownloadRequest) {
        let (lock, cvar) = &*self.inner;
        let mut queue = lock.lock().unwrap();
        queue.push_back(request);
        cvar.notify_one();
    }

    fn enqueue_front_promote(&self, request: DownloadRequest) -> bool {
        let (lock, cvar) = &*self.inner;
        let mut queue = lock.lock().unwrap();
        let original_len = queue.len();
        queue.retain(|existing| existing.uri != request.uri);
        let promoted = queue.len() != original_len;
        queue.push_front(request);
        cvar.notify_one();
        promoted
    }

    fn pop_front(&self) -> DownloadRequest {
        let (lock, cvar) = &*self.inner;
        let mut queue = lock.lock().unwrap();
        loop {
            if let Some(request) = queue.pop_front() {
                return request;
            }
            queue = cvar.wait(queue).unwrap();
        }
    }

    #[cfg(test)]
    fn snapshot(&self) -> Vec<DownloadRequest> {
        self.inner.0.lock().unwrap().iter().cloned().collect()
    }
}

/// Manages a queue of background downloads via yt-dlp.
pub struct DownloadManager {
    queue: DownloadWorkQueue,
    pending_uris: Arc<Mutex<HashSet<String>>>,
    progress: DownloadProgressMap,
    pending_queue_path: PathBuf,
}

impl DownloadManager {
    /// Create a new manager and spawn the background download thread.
    pub fn new(favorites: Arc<Mutex<FavoritesManager>>, app_state: Arc<Mutex<AppState>>) -> Self {
        let queue = DownloadWorkQueue::new();
        let thread_queue = queue.clone();
        let pending_uris = Arc::new(Mutex::new(HashSet::new()));
        let pending_clone = Arc::clone(&pending_uris);
        let progress: DownloadProgressMap = Arc::new(Mutex::new(HashMap::new()));
        let progress_clone = Arc::clone(&progress);
        let pending_queue_path = pending_download_queue_path();
        let thread_pending_queue_path = pending_queue_path.clone();
        let restore_favorites = Arc::clone(&favorites);

        std::thread::Builder::new()
            .name("download".into())
            .spawn(move || {
                download_loop(
                    thread_queue,
                    favorites,
                    pending_clone,
                    app_state,
                    progress_clone,
                    thread_pending_queue_path,
                );
            })
            .expect("spawn download thread");

        let manager = Self {
            queue,
            pending_uris,
            progress,
            pending_queue_path,
        };
        manager.restore_pending_downloads();
        manager.restore_incomplete_favorite_downloads(&restore_favorites);
        manager
    }

    /// Get a reference to the shared progress map for UI rendering.
    pub fn progress(&self) -> &DownloadProgressMap {
        &self.progress
    }

    /// Queue a download request. Deduplicates by URI. Non-blocking.
    pub fn enqueue(&self, request: DownloadRequest) {
        let mut pending = self.pending_uris.lock().unwrap();
        if pending.contains(&request.uri) {
            eprintln!("download: already queued, skipping: {}", request.uri);
            return;
        }
        pending.insert(request.uri.clone());
        let pending_count = pending.len();
        drop(pending);

        let uri = request.uri.clone();
        let artist_name = request.artist_name.clone();
        let track_name = request.track_name.clone();
        // Mark as queued in progress map immediately so UI shows it
        self.progress
            .lock()
            .unwrap()
            .insert(uri.clone(), DownloadPhase::Queued);
        persist_pending_download_to(&self.pending_queue_path, &request, QueuePlacement::Back);
        self.queue.enqueue_back(request);

        eprintln!(
            "download: queued uri={} track={} - {} pending={}",
            uri, artist_name, track_name, pending_count
        );
    }

    /// Queue a retry at the front. Existing queued work for the URI is promoted.
    pub fn retry_now(&self, request: DownloadRequest) {
        let mut pending = self.pending_uris.lock().unwrap();
        let already_pending = pending.contains(&request.uri);
        if !already_pending {
            pending.insert(request.uri.clone());
        }
        let pending_count = pending.len();
        drop(pending);

        let uri = request.uri.clone();
        let artist_name = request.artist_name.clone();
        let track_name = request.track_name.clone();
        self.progress
            .lock()
            .unwrap()
            .insert(uri.clone(), DownloadPhase::Queued);
        persist_pending_download_to(&self.pending_queue_path, &request, QueuePlacement::Front);
        let promoted = self.queue.enqueue_front_promote(request);

        eprintln!(
            "download: retry queued front uri={} track={} - {} pending={} already_pending={} promoted={}",
            uri, artist_name, track_name, pending_count, already_pending, promoted
        );
    }

    fn restore_pending_downloads(&self) {
        let restored_count = restore_pending_downloads_from(
            &self.pending_queue_path,
            &self.queue,
            &self.pending_uris,
            &self.progress,
        );
        if restored_count > 0 {
            eprintln!("download: restored {restored_count} pending download(s)");
        }
    }

    fn restore_incomplete_favorite_downloads(&self, favorites: &Arc<Mutex<FavoritesManager>>) {
        let restored_count = restore_incomplete_favorite_downloads_from(
            favorites,
            &self.pending_queue_path,
            &self.queue,
            &self.pending_uris,
            &self.progress,
        );
        if restored_count > 0 {
            eprintln!("download: restored {restored_count} incomplete favorite download(s)");
        }
    }
}

fn build_search_query(request: &DownloadRequest) -> String {
    format!("{} - {}", request.artist_name, request.track_name)
}

fn pending_download_queue_path() -> PathBuf {
    app_paths().data_dir.join("download_queue.json")
}

fn load_pending_downloads_from(path: &Path) -> Vec<DownloadRequest> {
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(_) => return Vec::new(),
    };

    match serde_json::from_str::<Vec<DownloadRequest>>(&data) {
        Ok(requests) => requests,
        Err(e) => {
            eprintln!(
                "download: pending queue parse error path={} error={e}",
                path.display()
            );
            Vec::new()
        }
    }
}

fn save_pending_downloads_to(path: &Path, requests: &[DownloadRequest]) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let tmp_path = path.with_extension("json.tmp");
    let json = match serde_json::to_string_pretty(requests) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("download: pending queue serialize error: {e}");
            return;
        }
    };

    if let Err(e) = fs::write(&tmp_path, json) {
        eprintln!(
            "download: pending queue write failed path={} error={e}",
            tmp_path.display()
        );
        return;
    }
    if let Err(e) = fs::rename(&tmp_path, path) {
        eprintln!(
            "download: pending queue rename failed path={} error={e}",
            path.display()
        );
    }
}

fn persist_pending_download_to(path: &Path, request: &DownloadRequest, placement: QueuePlacement) {
    let mut requests = load_pending_downloads_from(path);
    requests.retain(|existing| existing.uri != request.uri);
    match placement {
        QueuePlacement::Back => requests.push(request.clone()),
        QueuePlacement::Front => requests.insert(0, request.clone()),
    }
    save_pending_downloads_to(path, &requests);
}

fn remove_pending_download_from(path: &Path, uri: &str) {
    let mut requests = load_pending_downloads_from(path);
    let original_len = requests.len();
    requests.retain(|request| request.uri != uri);
    if requests.len() != original_len {
        save_pending_downloads_to(path, &requests);
    }
}

fn restore_pending_downloads_from(
    path: &Path,
    queue: &DownloadWorkQueue,
    pending_uris: &Arc<Mutex<HashSet<String>>>,
    progress: &DownloadProgressMap,
) -> usize {
    let restored = load_pending_downloads_from(path);
    if restored.is_empty() {
        return 0;
    }

    let mut restored_count = 0;
    for request in restored {
        if request.uri.trim().is_empty()
            || request.track_name.trim().is_empty()
            || request.artist_name.trim().is_empty()
        {
            continue;
        }

        let mut pending = pending_uris.lock().unwrap();
        if pending.contains(&request.uri) {
            continue;
        }
        pending.insert(request.uri.clone());
        drop(pending);

        progress
            .lock()
            .unwrap()
            .insert(request.uri.clone(), DownloadPhase::Queued);

        queue.enqueue_back(request);
        restored_count += 1;
    }

    restored_count
}

pub(crate) fn download_request_for_incomplete_favorite(
    entry: &FavoriteEntry,
) -> Option<DownloadRequest> {
    use crate::favorites::FavoriteSource;

    if entry.source != FavoriteSource::Spotify {
        return None;
    }
    if entry.downloaded
        && entry
            .file_path
            .as_deref()
            .is_some_and(|path| !path.trim().is_empty())
    {
        return None;
    }
    if entry.uri.trim().is_empty() || entry.name.trim().is_empty() || entry.artist.trim().is_empty()
    {
        return None;
    }

    Some(DownloadRequest {
        uri: entry.uri.clone(),
        track_name: entry.name.clone(),
        artist_name: entry.artist.clone(),
        cover_url: entry.cover_url.clone(),
        spotify_duration_ms: entry.spotify_duration_ms,
    })
}

fn restore_incomplete_favorite_downloads_from(
    favorites: &Arc<Mutex<FavoritesManager>>,
    path: &Path,
    queue: &DownloadWorkQueue,
    pending_uris: &Arc<Mutex<HashSet<String>>>,
    progress: &DownloadProgressMap,
) -> usize {
    let requests: Vec<DownloadRequest> = favorites
        .lock()
        .unwrap()
        .all_entries()
        .iter()
        .filter_map(download_request_for_incomplete_favorite)
        .collect();

    let mut restored_count = 0;
    for request in requests {
        let mut pending = pending_uris.lock().unwrap();
        if pending.contains(&request.uri) {
            continue;
        }
        pending.insert(request.uri.clone());
        drop(pending);

        progress
            .lock()
            .unwrap()
            .insert(request.uri.clone(), DownloadPhase::Queued);
        persist_pending_download_to(path, &request, QueuePlacement::Back);

        queue.enqueue_back(request);
        restored_count += 1;
    }

    restored_count
}

/// Sanitize a string for use as a filename.
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// Multi-candidate search & scoring
// ---------------------------------------------------------------------------

/// Apply common yt-dlp options: JS runtime and optional cookies.
fn apply_ytdlp_opts(cmd: &mut Command, cookies: Option<&Path>) {
    cmd.args([
        "--socket-timeout",
        "15",
        "--retries",
        "2",
        "--fragment-retries",
        "2",
    ]);
    let node = Path::new(NODE_BIN);
    if node.exists() {
        cmd.args(["--js-runtimes", &format!("node:{}", NODE_BIN)]);
    }
    if let Some(cookie_path) = cookies {
        cmd.args(["--cookies", &cookie_path.to_string_lossy()]);
    }
}

struct YtdlpTempDir {
    path: PathBuf,
}

impl YtdlpTempDir {
    fn new() -> std::io::Result<Self> {
        Self::new_at(app_paths().data_dir.join(YTDLP_TEMP_SUBDIR))
    }

    fn new_at(path: PathBuf) -> std::io::Result<Self> {
        fs::create_dir_all(&path)?;
        cleanup_pyinstaller_temp_dirs(&path);
        Ok(Self { path })
    }

    fn apply_to(&self, cmd: &mut Command) {
        apply_ytdlp_temp_env(cmd, &self.path);
    }
}

impl Drop for YtdlpTempDir {
    fn drop(&mut self) {
        cleanup_pyinstaller_temp_dirs(&self.path);
    }
}

fn apply_ytdlp_temp_env(cmd: &mut Command, temp_dir: &Path) {
    cmd.env("TMPDIR", temp_dir);
    cmd.env("TEMP", temp_dir);
    cmd.env("TMP", temp_dir);
}

fn cleanup_pyinstaller_temp_dirs(temp_dir: &Path) {
    let entries = match fs::read_dir(temp_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if !file_name.starts_with("_MEI") {
            continue;
        }
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Resolve yt-dlp cookies path if the file exists.
fn resolve_cookies_path() -> Option<PathBuf> {
    let path = app_paths().yt_dlp_cookies_path.clone();
    if path.exists() {
        eprintln!("download: cookies found path={}", path.display());
        Some(path)
    } else {
        None
    }
}

/// Search YouTube for candidates using yt-dlp metadata extraction (no download).
fn search_candidates(
    query: &str,
    count: usize,
    cookies: Option<&Path>,
) -> Result<Vec<SearchCandidate>, DownloadFailureKind> {
    let search_term = format!("ytsearch{}:{}", count, query);
    let ytdlp_temp = match YtdlpTempDir::new() {
        Ok(temp) => temp,
        Err(error) => {
            eprintln!("download: candidate search temp dir unavailable: {error}");
            return Err(DownloadFailureKind::TempStorage);
        }
    };
    let mut cmd = Command::new(YTDLP_BIN);
    cmd.args(["--dump-single-json", "--flat-playlist", "--no-warnings"]);
    apply_ytdlp_opts(&mut cmd, cookies);
    ytdlp_temp.apply_to(&mut cmd);
    cmd.arg(&search_term);

    eprintln!("download: searching candidates count={count} query={query}");

    let output = match command_output_with_timeout(cmd, YTDLP_SEARCH_TIMEOUT) {
        Ok(Some(output)) => output,
        Ok(None) => {
            eprintln!("download: candidate search timed out query={query}");
            return Err(DownloadFailureKind::Network);
        }
        Err(e) => {
            eprintln!("download: candidate search failed to launch: {e}");
            return Err(classify_ytdlp_launch_error(&e));
        }
    };

    if !output.status.success() {
        eprintln!(
            "download: candidate search failed status={} stderr={}",
            exit_status_label(&output.status),
            summarize_command_output(&output.stderr)
        );
        return Err(classify_download_failure_bytes(&output.stderr));
    }

    Ok(parse_candidates_json(&output.stdout))
}

/// Parse yt-dlp --dump-single-json output into candidates.
fn parse_candidates_json(json_bytes: &[u8]) -> Vec<SearchCandidate> {
    let json_str = String::from_utf8_lossy(json_bytes);
    let val: serde_json::Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("download: candidate JSON parse error: {e}");
            return Vec::new();
        }
    };

    let entries = match val.get("entries").and_then(|e| e.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    entries
        .iter()
        .filter_map(|entry| {
            let id = entry
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let title = entry.get("title").and_then(|v| v.as_str())?.to_string();
            if title.trim().is_empty() {
                return None;
            }
            let duration_secs = entry.get("duration").and_then(|v| v.as_f64());
            let channel = entry
                .get("channel")
                .or_else(|| entry.get("uploader"))
                .and_then(|v| v.as_str())
                .map(String::from);
            Some(SearchCandidate {
                id,
                title,
                duration_secs,
                channel,
            })
        })
        .collect()
}

/// Score a candidate against the download request. Higher is better.
fn score_candidate(candidate: &SearchCandidate, request: &DownloadRequest) -> f64 {
    let mut score = 0.0;

    // Duration similarity (40 points max) — strongest signal
    if let (Some(cand_secs), Some(spotify_ms)) =
        (candidate.duration_secs, request.spotify_duration_ms)
    {
        let cand_ms = (cand_secs * 1000.0) as i64;
        let diff_ms = (spotify_ms - cand_ms).abs();
        score += if diff_ms <= 2_000 {
            40.0
        } else if diff_ms <= 5_000 {
            30.0
        } else if diff_ms <= 10_000 {
            15.0
        } else if diff_ms <= 30_000 {
            5.0
        } else {
            0.0
        };
    }

    // Title similarity (25 points max)
    score += title_similarity(&candidate.title, &request.track_name) * 25.0;

    // Channel quality — " - Topic" channels are official label uploads (15 points)
    if let Some(ref ch) = candidate.channel {
        if ch.ends_with(" - Topic") {
            score += 15.0;
        }
    }

    // Negative signals — penalize covers/remixes unless the Spotify title has them too
    let cand_lower = candidate.title.to_lowercase();
    let req_lower = request.track_name.to_lowercase();
    for keyword in &[
        "cover",
        "remix",
        "live",
        "karaoke",
        "instrumental",
        "acoustic",
    ] {
        if cand_lower.contains(keyword) && !req_lower.contains(keyword) {
            score -= 15.0;
        }
    }

    score
}

/// Word-level Jaccard-like similarity between two titles.
/// Returns 0.0..=1.0 based on what fraction of reference words appear in the candidate.
fn title_similarity(candidate_title: &str, reference_title: &str) -> f64 {
    let normalize = |s: &str| -> HashSet<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(String::from)
            .collect()
    };
    let candidate_words = normalize(candidate_title);
    let reference_words = normalize(reference_title);
    if reference_words.is_empty() {
        return 0.0;
    }
    let intersection = candidate_words.intersection(&reference_words).count();
    intersection as f64 / reference_words.len() as f64
}

fn prefer_download_failure(
    current: DownloadFailureKind,
    next: DownloadFailureKind,
) -> DownloadFailureKind {
    if next.priority() >= current.priority() {
        next
    } else {
        current
    }
}

fn classify_ytdlp_launch_error(error: &std::io::Error) -> DownloadFailureKind {
    if error.kind() == std::io::ErrorKind::NotFound {
        DownloadFailureKind::MissingYtDlp
    } else {
        classify_download_failure(&format!("failed to run yt-dlp: {error}"))
    }
}

fn classify_download_failure_bytes(output: &[u8]) -> DownloadFailureKind {
    classify_download_failure(&String::from_utf8_lossy(output))
}

fn classify_download_failure(details: &str) -> DownloadFailureKind {
    let lower = details.to_lowercase();
    let has_any = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));

    if lower.contains("yt-dlp")
        && has_any(&[
            "no such file or directory",
            "not found",
            "cannot find",
            "permission denied",
        ])
    {
        return DownloadFailureKind::MissingYtDlp;
    }

    if has_any(&["ffmpeg", "ffprobe", "transcoder", FFMPEG_TRANSCODER_BIN])
        && has_any(&[
            "no such file or directory",
            "not found",
            "cannot find",
            "not installed",
            "unable to execute",
            "ffmpeg-location",
        ])
    {
        return DownloadFailureKind::MissingTranscoder;
    }

    if has_any(&["libmp3lame", "pcm_s16le", "s16le", "audio tool"]) {
        return DownloadFailureKind::MissingTranscoder;
    }

    if has_any(&[
        "no space left on device",
        "pyinstaller",
        "failed to extract",
        "could not create temporary",
        "tmpdir",
        "unpack",
        "unpacking",
    ]) {
        return DownloadFailureKind::TempStorage;
    }

    if has_any(&[
        "not a bot",
        "sign in to confirm",
        "cookies-from-browser",
        "use --cookies",
        "use cookies",
        "confirm you're not a bot",
    ]) {
        return DownloadFailureKind::CookiesBotCheck;
    }

    if has_any(&[
        "signature solving failed",
        "challenge solving failed",
        "po token",
        "gvs po token",
        "n challenge",
        "nsig",
    ]) {
        return DownloadFailureKind::SignatureChallenge;
    }

    if has_any(&[
        "temporary failure in name resolution",
        "name resolution",
        "could not resolve",
        "dns",
        "timed out",
        "timeout",
        "network is unreachable",
        "connection refused",
        "connection reset",
        "tls",
        "ssl",
        "certificate",
        "handshake",
    ]) {
        return DownloadFailureKind::Network;
    }

    if has_any(&[
        "requested format is not available",
        "no video formats",
        "no suitable format",
        "no matching audio",
        "duration mismatch",
    ]) {
        return DownloadFailureKind::NoMatchingAudio;
    }

    DownloadFailureKind::Generic
}

fn spawn_limited_capture_thread<R: Read + Send + 'static>(
    reader: R,
) -> std::thread::JoinHandle<Vec<u8>> {
    spawn_capture_thread_with_limit(reader, MAX_CAPTURED_STDERR_BYTES)
}

fn spawn_capture_thread_with_limit<R: Read + Send + 'static>(
    mut reader: R,
    limit: usize,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut captured = Vec::new();
        let mut buf = [0_u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let remaining = limit.saturating_sub(captured.len());
                    if remaining > 0 {
                        captured.extend_from_slice(&buf[..n.min(remaining)]);
                    }
                }
                Err(_) => break,
            }
        }
        captured
    })
}

fn collect_captured_output(handle: Option<std::thread::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    handle.and_then(|h| h.join().ok()).unwrap_or_default()
}

fn apply_ytdlp_progress_line(
    uri: &str,
    line: &str,
    progress: &DownloadProgressMap,
    app_state: &Arc<Mutex<AppState>>,
) {
    if let Some(pct) = parse_ytdlp_progress(line) {
        progress
            .lock()
            .unwrap()
            .insert(uri.to_string(), DownloadPhase::Downloading(pct));
        mark_dirty(app_state);
    }
}

fn spawn_ytdlp_progress_thread<R: Read + Send + 'static>(
    reader: R,
    uri: String,
    progress: DownloadProgressMap,
    app_state: Arc<Mutex<AppState>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines().map_while(Result::ok) {
            apply_ytdlp_progress_line(&uri, &line, &progress, &app_state);
        }
    })
}

// ---------------------------------------------------------------------------
// Post-download validation
// ---------------------------------------------------------------------------

/// Validate a downloaded track by comparing ffprobe duration with Spotify duration.
/// Returns the measured duration on success, or an error description.
fn validate_downloaded_track(
    file: &Path,
    spotify_duration_ms: Option<i64>,
) -> Result<Option<i64>, String> {
    let file_duration_ms = match probe_duration(file) {
        Some(d) => d,
        None => return Ok(None), // ffprobe unavailable — skip validation
    };

    if let Some(expected) = spotify_duration_ms {
        let diff = (expected - file_duration_ms).abs();
        if diff > DURATION_REJECT_THRESHOLD_MS {
            return Err(format!(
                "duration mismatch: spotify={}ms file={}ms diff={}ms threshold={}ms",
                expected, file_duration_ms, diff, DURATION_REJECT_THRESHOLD_MS
            ));
        }
        eprintln!(
            "download: duration validated spotify={}ms file={}ms diff={}ms",
            expected, file_duration_ms, diff
        );
    }

    Ok(Some(file_duration_ms))
}

// ---------------------------------------------------------------------------
// Download loop
// ---------------------------------------------------------------------------

/// Background loop that processes download requests one at a time.
fn download_loop(
    queue: DownloadWorkQueue,
    favorites: Arc<Mutex<FavoritesManager>>,
    pending_uris: Arc<Mutex<HashSet<String>>>,
    app_state: Arc<Mutex<AppState>>,
    progress: DownloadProgressMap,
    pending_queue_path: PathBuf,
) {
    loop {
        let req = queue.pop_front();
        wait_for_spotify_idle_before_download(&req.uri, &app_state);

        eprintln!(
            "download: starting uri={} track={} - {} spotify_duration={:?}ms",
            req.uri, req.artist_name, req.track_name, req.spotify_duration_ms
        );

        {
            let fav = favorites.lock().unwrap();
            if !fav.is_favorited(&req.uri) {
                eprintln!("download: skipping (unfavorited): {}", req.uri);
                clear_download_bookkeeping(
                    &req.uri,
                    &pending_uris,
                    &progress,
                    &app_state,
                    &pending_queue_path,
                );
                continue;
            }
            if fav.find_by_uri(&req.uri).is_some_and(|e| e.downloaded) {
                eprintln!("download: skipping (already downloaded): {}", req.uri);
                clear_download_bookkeeping(
                    &req.uri,
                    &pending_uris,
                    &progress,
                    &app_state,
                    &pending_queue_path,
                );
                continue;
            }
        }

        // Move to searching phase
        progress
            .lock()
            .unwrap()
            .insert(req.uri.clone(), DownloadPhase::Searching);
        mark_dirty(&app_state);

        let music_dir = app_paths().music_dir.clone();
        let _ = std::fs::create_dir_all(&music_dir);

        let safe_artist = sanitize_filename(&req.artist_name);
        let safe_track = sanitize_filename(&req.track_name);
        let base_name = format!("{} - {}", safe_artist, safe_track);
        let output_path = music_dir.join(format!("{}.mp3", base_name));
        let cover_path = music_dir.join(format!("{}.jpg", base_name));
        eprintln!(
            "download: target uri={} mp3={} cover={}",
            req.uri,
            output_path.display(),
            cover_path.display()
        );

        {
            let is_downloaded = favorites
                .lock()
                .unwrap()
                .find_by_uri(&req.uri)
                .is_some_and(|e| e.downloaded);
            if !is_downloaded {
                cleanup_stale_files(&output_path, &base_name, &music_dir);
            }
        }

        let search_query = build_search_query(&req);
        let output_template = output_path.to_string_lossy().to_string();
        let cookies = resolve_cookies_path();

        let candidate_search =
            search_candidates(&search_query, CANDIDATE_COUNT, cookies.as_deref());

        let attempt = DownloadAttemptContext {
            output_template: &output_template,
            output_path: &output_path,
            cookies: cookies.as_deref(),
            favorites: &favorites,
            progress: &progress,
            app_state: &app_state,
        };

        let outcome = match candidate_search {
            Err(kind) => DownloadOutcome::Failed(kind),
            Ok(candidates) if candidates.is_empty() => {
                eprintln!("download: no candidates found, falling back to direct search");
                try_direct_download(&req, &search_query, &attempt)
            }
            Ok(candidates) => {
                let mut scored: Vec<(f64, &SearchCandidate)> = candidates
                    .iter()
                    .map(|c| (score_candidate(c, &req), c))
                    .collect();
                scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

                for (i, (sc, cand)) in scored.iter().enumerate() {
                    eprintln!(
                        "download: candidate #{} score={:.1} id={} title=\"{}\" duration={:.1}s channel={:?}",
                        i + 1,
                        sc,
                        cand.id,
                        cand.title,
                        cand.duration_secs.unwrap_or(0.0),
                        cand.channel
                    );
                }

                try_candidates_download(&req, &scored, &attempt)
            }
        };

        let pending_queue_action = match outcome {
            DownloadOutcome::Success => {
                finalize_download(&req, &output_path, &cover_path, &favorites);
                PendingQueueAction::Remove
            }
            DownloadOutcome::Skipped => PendingQueueAction::Remove,
            DownloadOutcome::Failed(kind) => {
                if output_path.exists() {
                    let _ = std::fs::remove_file(&output_path);
                    eprintln!(
                        "download: removed failed partial uri={} path={}",
                        req.uri,
                        output_path.display()
                    );
                }
                eprintln!(
                    "download: giving up uri={} track={} - {} reason={:?}",
                    req.uri, req.artist_name, req.track_name, kind
                );
                show_final_download_failure_notice(&outcome, &app_state);
                PendingQueueAction::KeepForRetry
            }
        };

        // Clear progress entry and notify UI
        finish_download_bookkeeping(
            &req.uri,
            &pending_uris,
            &progress,
            &app_state,
            &pending_queue_path,
            pending_queue_action,
        );
    }
}

fn wait_for_spotify_idle_before_download(uri: &str, app_state: &Arc<Mutex<AppState>>) {
    let mut logged = false;
    loop {
        let defer = app_state
            .lock()
            .map(|state| should_defer_download_for_spotify(&state))
            .unwrap_or(false);
        if !defer {
            if logged {
                eprintln!("download: resumed after Spotify idle uri={uri}");
            }
            return;
        }
        if !logged {
            eprintln!("download: deferred while Spotify playing uri={uri}");
            logged = true;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn should_defer_download_for_spotify(state: &AppState) -> bool {
    state.mode == AppMode::Spotify && !state.paused
}

fn mark_dirty(app_state: &Arc<Mutex<AppState>>) {
    if let Ok(mut st) = app_state.lock() {
        st.render_dirty = true;
    }
}

fn clear_download_bookkeeping(
    uri: &str,
    pending_uris: &Arc<Mutex<HashSet<String>>>,
    progress: &DownloadProgressMap,
    app_state: &Arc<Mutex<AppState>>,
    pending_queue_path: &Path,
) {
    finish_download_bookkeeping(
        uri,
        pending_uris,
        progress,
        app_state,
        pending_queue_path,
        PendingQueueAction::Remove,
    );
}

fn finish_download_bookkeeping(
    uri: &str,
    pending_uris: &Arc<Mutex<HashSet<String>>>,
    progress: &DownloadProgressMap,
    app_state: &Arc<Mutex<AppState>>,
    pending_queue_path: &Path,
    pending_queue_action: PendingQueueAction,
) {
    progress.lock().unwrap().remove(uri);
    pending_uris.lock().unwrap().remove(uri);
    match pending_queue_action {
        PendingQueueAction::Remove => remove_pending_download_from(pending_queue_path, uri),
        PendingQueueAction::KeepForRetry => {
            eprintln!("download: kept pending queue for next launch retry uri={uri}");
        }
    }
    mark_dirty(app_state);
}

fn show_final_download_failure_notice(outcome: &DownloadOutcome, app_state: &Arc<Mutex<AppState>>) {
    if let DownloadOutcome::Failed(kind) = outcome {
        if let Ok(mut st) = app_state.lock() {
            st.show_notice(kind.notice(), std::time::Instant::now());
        }
    }
}

struct DownloadAttemptContext<'a> {
    output_template: &'a str,
    output_path: &'a Path,
    cookies: Option<&'a Path>,
    favorites: &'a Arc<Mutex<FavoritesManager>>,
    progress: &'a DownloadProgressMap,
    app_state: &'a Arc<Mutex<AppState>>,
}

/// Try downloading each scored candidate in order, with post-download validation.
fn try_candidates_download(
    req: &DownloadRequest,
    scored: &[(f64, &SearchCandidate)],
    attempt: &DownloadAttemptContext<'_>,
) -> DownloadOutcome {
    let mut final_failure = DownloadFailureKind::NoMatchingAudio;
    for (rank, (score, cand)) in scored.iter().enumerate() {
        if !attempt.favorites.lock().unwrap().is_favorited(&req.uri) {
            eprintln!(
                "download: skipping (unfavorited during search): {}",
                req.uri
            );
            return DownloadOutcome::Skipped;
        }

        // Reset progress for each new candidate attempt
        attempt
            .progress
            .lock()
            .unwrap()
            .insert(req.uri.clone(), DownloadPhase::Downloading(0.0));
        mark_dirty(attempt.app_state);

        let yt_url = format!("https://www.youtube.com/watch?v={}", cand.id);
        eprintln!(
            "download: trying candidate #{} score={:.1} id={} uri={}",
            rank + 1,
            score,
            cand.id,
            req.uri
        );

        match download_single_url(
            &yt_url,
            attempt.output_template,
            attempt.cookies,
            &req.uri,
            attempt.progress,
            attempt.app_state,
            req.spotify_duration_ms,
        ) {
            DownloadOutcome::Success => {
                match validate_downloaded_track(attempt.output_path, req.spotify_duration_ms) {
                    Ok(_) => return DownloadOutcome::Success,
                    Err(reason) => {
                        eprintln!(
                            "download: candidate #{} rejected: {} uri={}",
                            rank + 1,
                            reason,
                            req.uri
                        );
                        final_failure = prefer_download_failure(
                            final_failure,
                            classify_download_failure(&reason),
                        );
                        let _ = std::fs::remove_file(attempt.output_path);
                    }
                }
            }
            DownloadOutcome::Skipped => return DownloadOutcome::Skipped,
            DownloadOutcome::Failed(kind) => {
                final_failure = prefer_download_failure(final_failure, kind);
                if attempt.output_path.exists() {
                    let _ = std::fs::remove_file(attempt.output_path);
                }
            }
        }
    }
    DownloadOutcome::Failed(final_failure)
}

/// Download audio from a specific YouTube URL, reporting progress via file size polling.
fn download_single_url(
    url: &str,
    output_template: &str,
    cookies: Option<&Path>,
    uri: &str,
    progress: &DownloadProgressMap,
    app_state: &Arc<Mutex<AppState>>,
    expected_duration_ms: Option<i64>,
) -> DownloadOutcome {
    let ytdlp_temp = match YtdlpTempDir::new() {
        Ok(temp) => temp,
        Err(error) => {
            eprintln!("download: yt-dlp temp dir unavailable: {error}");
            return DownloadOutcome::Failed(DownloadFailureKind::TempStorage);
        }
    };
    let mut cmd = Command::new(YTDLP_BIN);
    cmd.args([
        "-x",
        "--audio-format",
        "mp3",
        "--audio-quality",
        "5",
        "--no-playlist",
        "--ffmpeg-location",
        FFMPEG_TRANSCODER_BIN,
        "--newline",
        "--progress",
        "-o",
        output_template,
    ]);
    apply_ytdlp_opts(&mut cmd, cookies);
    ytdlp_temp.apply_to(&mut cmd);
    cmd.arg(url);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("download: failed to run yt-dlp: {e}");
            return DownloadOutcome::Failed(classify_ytdlp_launch_error(&e));
        }
    };
    let stderr_handle = child.stderr.take().map(spawn_limited_capture_thread);
    let stdout_handle = child.stdout.take().map(|stdout| {
        spawn_ytdlp_progress_thread(
            stdout,
            uri.to_string(),
            Arc::clone(progress),
            Arc::clone(app_state),
        )
    });

    // Poll file size in a background thread to estimate progress
    let poll_uri = uri.to_string();
    let poll_progress = Arc::clone(progress);
    let poll_app_state = Arc::clone(app_state);
    let poll_path = PathBuf::from(output_template);
    let poll_duration_ms = expected_duration_ms;
    let child_id = child.id();

    let poll_handle = std::thread::spawn(move || {
        // Estimate expected file size: ~128kbps mp3 at quality 5 ≈ 16KB/s
        let expected_bytes = poll_duration_ms
            .map(|ms| (ms as f64 / 1000.0 * 16_000.0) as u64)
            .unwrap_or(5_000_000);

        let poll_interval = std::time::Duration::from_millis(PROGRESS_THROTTLE_MS as u64);
        let mut saw_mp3 = false;

        loop {
            std::thread::sleep(poll_interval);

            // Check if yt-dlp process is still alive
            let alive = unsafe { libc::kill(child_id as i32, 0) } == 0;
            if !alive {
                break;
            }

            // Look for intermediate files (.webm, .m4a, .opus) or the final .mp3
            let mp3_exists = poll_path.exists();
            let base = poll_path.with_extension("");
            let intermediate_size: u64 = ["webm", "m4a", "opus", "part"]
                .iter()
                .filter_map(|ext| {
                    let p = base.with_extension(ext);
                    fs::metadata(&p).ok().map(|m| m.len())
                })
                .sum();

            if mp3_exists && !saw_mp3 {
                // mp3 appeared — transcoding phase
                saw_mp3 = true;
                poll_progress
                    .lock()
                    .unwrap()
                    .insert(poll_uri.clone(), DownloadPhase::Transcoding);
                mark_dirty(&poll_app_state);
            } else if intermediate_size > 0 && !saw_mp3 {
                // Still downloading intermediate format
                let pct = (intermediate_size as f32 / expected_bytes as f32).clamp(0.01, 0.95);
                poll_progress
                    .lock()
                    .unwrap()
                    .insert(poll_uri.clone(), DownloadPhase::Downloading(pct));
                mark_dirty(&poll_app_state);
            }
        }
    });

    match wait_child_with_timeout(&mut child, YTDLP_DOWNLOAD_TIMEOUT) {
        Ok(Some(status)) => {
            let _ = poll_handle.join();
            if let Some(handle) = stdout_handle {
                let _ = handle.join();
            }
            let stderr = collect_captured_output(stderr_handle);
            if status.success() {
                DownloadOutcome::Success
            } else {
                eprintln!(
                    "download: yt-dlp failed url={} status={} stderr={}",
                    url,
                    exit_status_label(&status),
                    summarize_command_output(&stderr)
                );
                DownloadOutcome::Failed(classify_download_failure_bytes(&stderr))
            }
        }
        Ok(None) => {
            eprintln!("download: yt-dlp timed out url={url}");
            let _ = poll_handle.join();
            if let Some(handle) = stdout_handle {
                let _ = handle.join();
            }
            let _ = collect_captured_output(stderr_handle);
            DownloadOutcome::Failed(DownloadFailureKind::Network)
        }
        Err(e) => {
            eprintln!("download: yt-dlp wait error: {e}");
            let _ = poll_handle.join();
            if let Some(handle) = stdout_handle {
                let _ = handle.join();
            }
            let _ = collect_captured_output(stderr_handle);
            DownloadOutcome::Failed(DownloadFailureKind::Generic)
        }
    }
}

fn wait_child_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if started.elapsed() >= timeout {
            kill_child_process_group(child.id());
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn kill_child_process_group(pid: u32) {
    #[cfg(unix)]
    unsafe {
        let _ = libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

fn command_output_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> std::io::Result<Option<Output>> {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    configure_child_process_group(&mut cmd);

    let mut child = cmd.spawn()?;
    let stdout_handle = child
        .stdout
        .take()
        .map(|stdout| spawn_capture_thread_with_limit(stdout, MAX_CAPTURED_STDOUT_BYTES));
    let stderr_handle = child.stderr.take().map(spawn_limited_capture_thread);

    match wait_child_with_timeout(&mut child, timeout)? {
        Some(status) => Ok(Some(Output {
            status,
            stdout: collect_captured_output(stdout_handle),
            stderr: collect_captured_output(stderr_handle),
        })),
        None => Ok(None),
    }
}

fn configure_child_process_group(cmd: &mut Command) {
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
}

/// Parse a yt-dlp progress line like "[download]  45.2% of 3.5MiB" into 0.0..1.0.
fn parse_ytdlp_progress(line: &str) -> Option<f32> {
    let line = line.trim();
    if !line.starts_with("[download]") {
        return None;
    }
    // Find the percentage: look for a number followed by '%'
    let after_tag = &line["[download]".len()..];
    let pct_pos = after_tag.find('%')?;
    let num_str = after_tag[..pct_pos].trim();
    let pct: f32 = num_str.parse().ok()?;
    Some((pct / 100.0).clamp(0.0, 1.0))
}

/// Fallback: direct ytsearch1 download (used when candidate search returns nothing).
fn try_direct_download(
    req: &DownloadRequest,
    search_query: &str,
    ctx: &DownloadAttemptContext<'_>,
) -> DownloadOutcome {
    let mut final_failure = DownloadFailureKind::Generic;
    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            eprintln!(
                "download: retry {attempt}/{MAX_RETRIES} for {} - {}",
                req.artist_name, req.track_name
            );
            std::thread::sleep(std::time::Duration::from_secs(RETRY_DELAY_SECS));

            if !ctx.favorites.lock().unwrap().is_favorited(&req.uri) {
                eprintln!("download: skipping retry (unfavorited): {}", req.uri);
                return DownloadOutcome::Skipped;
            }
        }

        // Reset progress for retry
        ctx.progress
            .lock()
            .unwrap()
            .insert(req.uri.clone(), DownloadPhase::Downloading(0.0));
        mark_dirty(ctx.app_state);

        eprintln!(
            "download: direct search attempt={}/{} uri={} query={}",
            attempt + 1,
            MAX_RETRIES + 1,
            req.uri,
            search_query,
        );

        let search_url = format!("ytsearch1:{}", search_query);
        match download_single_url(
            &search_url,
            ctx.output_template,
            ctx.cookies,
            &req.uri,
            ctx.progress,
            ctx.app_state,
            req.spotify_duration_ms,
        ) {
            DownloadOutcome::Success => {
                match validate_downloaded_track(ctx.output_path, req.spotify_duration_ms) {
                    Ok(_) => return DownloadOutcome::Success,
                    Err(reason) => {
                        eprintln!(
                            "download: direct download rejected: {} uri={}",
                            reason, req.uri
                        );
                        final_failure = prefer_download_failure(
                            final_failure,
                            classify_download_failure(&reason),
                        );
                        let _ = std::fs::remove_file(ctx.output_path);
                    }
                }
            }
            DownloadOutcome::Skipped => return DownloadOutcome::Skipped,
            DownloadOutcome::Failed(kind) => {
                final_failure = prefer_download_failure(final_failure, kind);
                if ctx.output_path.exists() {
                    let _ = std::fs::remove_file(ctx.output_path);
                }
            }
        }
    }
    DownloadOutcome::Failed(final_failure)
}

/// Finalize a successful download: probe duration, update favorites, fetch cover.
fn finalize_download(
    req: &DownloadRequest,
    output_path: &Path,
    cover_path: &Path,
    favorites: &Arc<Mutex<FavoritesManager>>,
) {
    let size = fs::metadata(output_path)
        .map(|meta| format_bytes(meta.len()))
        .unwrap_or_else(|_| "unknown".to_string());
    eprintln!(
        "download: success uri={} mp3={} size={}",
        req.uri,
        output_path.display(),
        size
    );

    let duration_ms = probe_duration(output_path);

    let mut fav = favorites.lock().unwrap();
    fav.mark_downloaded(&req.uri, &output_path.to_string_lossy(), duration_ms);
    eprintln!(
        "download: library updated uri={} duration_ms={:?}",
        req.uri, duration_ms
    );

    let cover_downloaded = download_cover(&req.cover_url, cover_path);
    if !cover_path.exists() && !req.cover_url.is_empty() {
        let copied = try_copy_from_cover_cache(&req.cover_url, cover_path);
        if !cover_downloaded && !copied {
            eprintln!("download: cover unavailable uri={}", req.uri);
        }
    } else if req.cover_url.is_empty() {
        eprintln!("download: no cover url for uri={}", req.uri);
    }
    if cover_path.exists() {
        fav.set_cover_path(&req.uri, &cover_path.to_string_lossy());
        eprintln!(
            "download: cover ready uri={} path={}",
            req.uri,
            cover_path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Remove stale partial files left by a previous interrupted download.
/// Cleans up the target mp3 and any yt-dlp intermediate files sharing the same base name.
fn cleanup_stale_files(output_path: &Path, base_name: &str, music_dir: &Path) {
    let stale_extensions = ["mp3", "webm", "m4a", "opus", "ogg", "wav", "part"];
    for ext in &stale_extensions {
        let path = music_dir.join(format!("{}.{}", base_name, ext));
        if path.exists() {
            eprintln!("download: removing stale file: {}", path.display());
            let _ = std::fs::remove_file(&path);
        }
    }
    // Also check for .mp3.part (yt-dlp partial download marker)
    let part_path = PathBuf::from(format!("{}.part", output_path.display()));
    if part_path.exists() {
        eprintln!("download: removing stale file: {}", part_path.display());
        let _ = std::fs::remove_file(&part_path);
    }
}

/// Use ffprobe to get track duration in milliseconds.
fn probe_duration(path: &Path) -> Option<i64> {
    let output = match Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            eprintln!(
                "download: ffprobe launch failed for {}: {e}",
                path.display()
            );
            return None;
        }
    };

    if !output.status.success() {
        eprintln!(
            "download: ffprobe failed for {} status={} stderr={}",
            path.display(),
            exit_status_label(&output.status),
            summarize_command_output(&output.stderr)
        );
        return None;
    }

    let s = String::from_utf8_lossy(&output.stdout);
    let secs: f64 = match s.trim().parse() {
        Ok(secs) => secs,
        Err(_) => {
            eprintln!(
                "download: ffprobe parse failed for {} stdout={}",
                path.display(),
                summarize_command_output(&output.stdout)
            );
            return None;
        }
    };
    let duration_ms = (secs * 1000.0) as i64;
    eprintln!(
        "download: ffprobe duration={}ms file={}",
        duration_ms,
        path.display()
    );
    Some(duration_ms)
}

/// Download cover art via curl (HTTPS support).
fn download_cover(url: &str, dest: &Path) -> bool {
    if url.is_empty() {
        return false;
    }
    if !is_allowed_cover_url(url) {
        eprintln!("download: rejected untrusted cover url={url}");
        return false;
    }

    let cert_file = crate::resources::find_resource("ca-certificates.crt");
    let cert_arg = cert_file.map(|p| p.to_string_lossy().to_string());

    let mut cmd = Command::new("curl");
    cmd.args(["-4", "-fsSL", "--connect-timeout", "5", "--max-time", "15"]);
    if let Some(ref cert) = cert_arg {
        cmd.args(["--cacert", cert]);
    }
    cmd.args(["-o"]).arg(dest).arg(url);

    match cmd.output() {
        Ok(output) => {
            if !output.status.success() {
                eprintln!(
                    "download: cover fetch failed url={} status={} stderr={}",
                    url,
                    exit_status_label(&output.status),
                    summarize_command_output(&output.stderr)
                );
                false
            } else {
                eprintln!(
                    "download: cover fetched url={} dest={}",
                    url,
                    dest.display()
                );
                true
            }
        }
        Err(e) => {
            eprintln!("download: curl error: {e}");
            false
        }
    }
}

/// Try to copy cover art from Spotify's local cover cache.
/// The cache stores original JPEG bytes keyed by FNV hash of the URL.
fn try_copy_from_cover_cache(url: &str, dest: &Path) -> bool {
    let mut hash = 0xcbf29ce484222325u64;
    for &byte in url.as_bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let cache_path = PathBuf::from("/tmp/sideb-cover-cache").join(format!("{hash:016x}.img"));

    if cache_path.exists() {
        match std::fs::copy(&cache_path, dest) {
            Ok(_) => {
                eprintln!(
                    "download: cover copied from cache {} -> {}",
                    cache_path.display(),
                    dest.display()
                );
                true
            }
            Err(e) => {
                eprintln!("download: cache copy failed: {e}");
                false
            }
        }
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::favorites::{FavoriteEntry, FavoriteSource, FavoritesManager};
    use crate::mode::AppMode;

    fn sample_request() -> DownloadRequest {
        DownloadRequest {
            uri: "spotify:track:123".to_string(),
            track_name: "Komm, Susser Tod".to_string(),
            artist_name: "Arianne".to_string(),
            cover_url: String::new(),
            spotify_duration_ms: Some(467_000),
        }
    }

    fn sample_favorite(uri: &str, source: FavoriteSource, downloaded: bool) -> FavoriteEntry {
        FavoriteEntry {
            uri: uri.to_string(),
            name: "Retry Me".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            cover_url: "https://i.scdn.co/image/example".to_string(),
            source,
            file_path: downloaded.then(|| format!("/tmp/{uri}.mp3")),
            cover_path: None,
            duration_ms: downloaded.then_some(123_000),
            spotify_duration_ms: Some(123_456),
            downloaded,
            added_at: "0".to_string(),
        }
    }

    #[test]
    fn legacy_download_query_uses_artist_dash_track_format() {
        let request = sample_request();
        assert_eq!(build_search_query(&request), "Arianne - Komm, Susser Tod");
    }

    // --- Title similarity ---

    #[test]
    fn title_similarity_exact_match_returns_one() {
        assert!(
            (title_similarity("Komm, Susser Tod", "Komm, Susser Tod") - 1.0).abs() < f64::EPSILON
        );
    }

    #[test]
    fn title_similarity_partial_overlap() {
        // "Komm" and "Tod" match out of 3 reference words → 2/3
        let sim = title_similarity("Komm Bitter Tod", "Komm, Susser Tod");
        assert!((sim - 2.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn title_similarity_no_overlap_returns_zero() {
        assert!(
            title_similarity("Something Else Entirely", "Komm, Susser Tod").abs() < f64::EPSILON
        );
    }

    #[test]
    fn title_similarity_empty_reference_returns_zero() {
        assert!(title_similarity("Hello", "").abs() < f64::EPSILON);
    }

    #[test]
    fn title_similarity_is_case_insensitive() {
        assert!(
            (title_similarity("KOMM SUSSER TOD", "komm susser tod") - 1.0).abs() < f64::EPSILON
        );
    }

    // --- Candidate scoring ---

    #[test]
    fn score_perfect_match_is_high() {
        let req = sample_request(); // 467_000ms
        let cand = SearchCandidate {
            id: "abc".into(),
            title: "Arianne - Komm, Susser Tod".into(),
            duration_secs: Some(467.0), // exact match
            channel: Some("Arianne - Topic".into()),
        };
        let score = score_candidate(&cand, &req);
        // 40 (duration) + 25 (all words match) + 15 (Topic) = 80
        assert!(score >= 75.0, "expected high score, got {score}");
    }

    #[test]
    fn score_wrong_duration_is_low() {
        let req = sample_request(); // 467_000ms
        let cand = SearchCandidate {
            id: "xyz".into(),
            title: "Komm, Susser Tod".into(),
            duration_secs: Some(600.0), // 133s off — way too long
            channel: None,
        };
        let score = score_candidate(&cand, &req);
        // 0 (duration >30s off) + 25 (title match) + 0 (no Topic) = 25
        assert!(
            score <= 30.0,
            "expected low score for wrong duration, got {score}"
        );
    }

    #[test]
    fn score_cover_version_is_penalized() {
        let req = sample_request();
        let cand = SearchCandidate {
            id: "cov".into(),
            title: "Komm, Susser Tod (Cover)".into(),
            duration_secs: Some(467.0),
            channel: None,
        };
        let score = score_candidate(&cand, &req);
        // 40 (duration) + ~25 (title) - 15 (cover penalty) + 0 = ~50
        // vs perfect match of ~80 — cover should rank lower
        assert!(score < 60.0, "cover should be penalized, got {score}");
    }

    #[test]
    fn score_topic_channel_beats_random_channel() {
        let req = sample_request();
        let topic = SearchCandidate {
            id: "t".into(),
            title: "Komm, Susser Tod".into(),
            duration_secs: Some(467.0),
            channel: Some("Arianne - Topic".into()),
        };
        let random = SearchCandidate {
            id: "r".into(),
            title: "Komm, Susser Tod".into(),
            duration_secs: Some(467.0),
            channel: Some("RandomUser123".into()),
        };
        assert!(score_candidate(&topic, &req) > score_candidate(&random, &req));
    }

    // --- Candidate JSON parsing ---

    #[test]
    fn parse_candidates_extracts_entries() {
        let json = r#"{
            "entries": [
                {"id": "abc123", "title": "Song Title", "duration": 245.5, "channel": "Artist - Topic"},
                {"id": "def456", "title": "Another Song", "duration": 180.0, "uploader": "SomeUser"},
                {"id": "ghi789", "title": "", "duration": 100.0}
            ]
        }"#;
        let candidates = parse_candidates_json(json.as_bytes());
        assert_eq!(candidates.len(), 2); // empty title is filtered
        assert_eq!(candidates[0].id, "abc123");
        assert_eq!(candidates[0].title, "Song Title");
        assert!((candidates[0].duration_secs.unwrap() - 245.5).abs() < 0.01);
        assert_eq!(candidates[0].channel.as_deref(), Some("Artist - Topic"));
        assert_eq!(candidates[1].channel.as_deref(), Some("SomeUser")); // uploader fallback
    }

    #[test]
    fn parse_candidates_handles_missing_entries() {
        let json = r#"{"type": "playlist"}"#;
        assert!(parse_candidates_json(json.as_bytes()).is_empty());
    }

    #[test]
    fn parse_candidates_handles_invalid_json() {
        assert!(parse_candidates_json(b"not json").is_empty());
    }

    #[test]
    fn wait_child_with_timeout_kills_slow_process() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 2"])
            .spawn()
            .expect("spawn sleep process");

        let status = wait_child_with_timeout(&mut child, Duration::from_millis(50))
            .expect("wait should not error");

        assert_eq!(status, None);
    }

    #[test]
    fn wait_child_with_timeout_returns_fast_exit_status() {
        let mut child = Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("spawn fast process");

        let status = wait_child_with_timeout(&mut child, Duration::from_secs(1))
            .expect("wait should not error")
            .expect("process should exit before timeout");

        assert!(status.success());
    }

    #[test]
    fn command_output_with_timeout_kills_slow_command() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 2"]);

        let output = command_output_with_timeout(cmd, Duration::from_millis(50))
            .expect("timeout wrapper should not error");

        assert!(output.is_none());
    }

    #[test]
    fn command_output_with_timeout_captures_fast_output() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf ok"]);

        let output = command_output_with_timeout(cmd, Duration::from_secs(1))
            .expect("timeout wrapper should not error")
            .expect("fast command should complete");

        assert!(output.status.success());
        assert_eq!(output.stdout, b"ok");
    }

    #[test]
    fn ytdlp_temp_guard_cleans_pyinstaller_extract_dirs() {
        let root = temp_test_dir("ytdlp-temp-cleanup");
        fs::create_dir_all(root.join("_MEIleak")).unwrap();
        fs::create_dir_all(root.join("keep")).unwrap();
        fs::write(root.join("_MEIfile"), b"keep file").unwrap();

        cleanup_pyinstaller_temp_dirs(&root);

        assert!(!root.join("_MEIleak").exists());
        assert!(root.join("keep").exists());
        assert!(root.join("_MEIfile").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ytdlp_temp_guard_sets_tmp_environment_for_child() {
        let root = temp_test_dir("ytdlp-temp-env");
        let guard = YtdlpTempDir::new_at(root.clone()).unwrap();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf '%s|%s|%s' \"$TMPDIR\" \"$TEMP\" \"$TMP\""]);
        guard.apply_to(&mut cmd);

        let output = command_output_with_timeout(cmd, Duration::from_secs(1))
            .expect("env command should run")
            .expect("env command should finish");

        assert!(output.status.success());
        let expected = format!("{}|{}|{}", root.display(), root.display(), root.display());
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected);

        let _ = fs::remove_dir_all(&root);
    }

    // --- Duration validation ---

    #[test]
    fn validate_passes_when_no_spotify_duration() {
        // Cannot test with real file, but verify the logic path
        // When spotify_duration_ms is None, validation should pass
        // (tested via the Ok path — actual ffprobe call would fail without a file)
        assert!(validate_downloaded_track(Path::new("/nonexistent"), None).is_ok());
    }

    // --- Progress parsing ---

    #[test]
    fn parse_progress_extracts_percentage() {
        let pct = parse_ytdlp_progress("[download]  45.2% of 3.5MiB at 1.2MiB/s").unwrap();
        assert!((pct - 0.452).abs() < 0.001);
    }

    #[test]
    fn parse_progress_handles_100_percent() {
        assert_eq!(parse_ytdlp_progress("[download] 100% of 3.5MiB"), Some(1.0));
    }

    #[test]
    fn parse_progress_ignores_non_download_lines() {
        assert_eq!(parse_ytdlp_progress("[info] Extracting URL: ..."), None);
    }

    #[test]
    fn parse_progress_ignores_destination_line() {
        assert_eq!(
            parse_ytdlp_progress("[download] Destination: file.webm"),
            None
        );
    }

    #[test]
    fn parse_progress_clamps_to_unit_range() {
        let result = parse_ytdlp_progress("[download] 0.0% of 1MiB");
        assert_eq!(result, Some(0.0));
    }

    #[test]
    fn ytdlp_progress_line_updates_download_progress() {
        let uri = "spotify:track:progress";
        let progress: DownloadProgressMap = Arc::new(Mutex::new(HashMap::new()));
        let app_state = Arc::new(Mutex::new(AppState::new()));

        apply_ytdlp_progress_line(
            uri,
            "[download]  45.2% of 3.5MiB at 1.2MiB/s",
            &progress,
            &app_state,
        );

        let phase = *progress.lock().unwrap().get(uri).unwrap();
        match phase {
            DownloadPhase::Downloading(pct) => assert!((pct - 0.452).abs() < 0.001),
            other => panic!("expected downloading progress, got {other:?}"),
        }
        assert!(app_state.lock().unwrap().render_dirty);
    }

    // --- Overall progress mapping ---

    #[test]
    fn overall_progress_queued_is_zero() {
        assert_eq!(DownloadPhase::Queued.overall_progress(), 0.0);
    }

    #[test]
    fn overall_progress_searching_is_in_first_quarter() {
        let p = DownloadPhase::Searching.overall_progress();
        assert!(p > 0.0 && p <= 0.25);
    }

    #[test]
    fn overall_progress_downloading_maps_to_25_75() {
        let start = DownloadPhase::Downloading(0.0).overall_progress();
        let mid = DownloadPhase::Downloading(0.5).overall_progress();
        let end = DownloadPhase::Downloading(1.0).overall_progress();
        assert!((start - 0.25).abs() < 0.001);
        assert!((mid - 0.50).abs() < 0.001);
        assert!((end - 0.75).abs() < 0.001);
    }

    #[test]
    fn overall_progress_transcoding_is_in_last_quarter() {
        let p = DownloadPhase::Transcoding.overall_progress();
        assert!((0.75..=1.0).contains(&p));
    }

    #[test]
    fn clear_download_bookkeeping_removes_progress_and_pending_state() {
        let uri = "spotify:track:queued";
        let dir = temp_test_dir("clear-download-bookkeeping");
        let queue_path = dir.join("download_queue.json");
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let progress: DownloadProgressMap = Arc::new(Mutex::new(HashMap::new()));
        let app_state = Arc::new(Mutex::new(AppState::new()));
        let request = DownloadRequest {
            uri: uri.to_string(),
            track_name: "Queued".to_string(),
            artist_name: "Artist".to_string(),
            cover_url: String::new(),
            spotify_duration_ms: None,
        };

        pending.lock().unwrap().insert(uri.to_string());
        progress
            .lock()
            .unwrap()
            .insert(uri.to_string(), DownloadPhase::Queued);

        save_pending_downloads_to(&queue_path, &[request]);

        clear_download_bookkeeping(uri, &pending, &progress, &app_state, &queue_path);

        assert!(!pending.lock().unwrap().contains(uri));
        assert!(!progress.lock().unwrap().contains_key(uri));
        assert!(load_pending_downloads_from(&queue_path).is_empty());
        assert!(app_state.lock().unwrap().render_dirty);
    }

    #[test]
    fn failed_download_bookkeeping_keeps_queue_for_next_launch_retry() {
        let uri = "spotify:track:retry";
        let dir = temp_test_dir("failed-download-bookkeeping");
        let queue_path = dir.join("download_queue.json");
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let progress: DownloadProgressMap = Arc::new(Mutex::new(HashMap::new()));
        let app_state = Arc::new(Mutex::new(AppState::new()));
        let request = DownloadRequest {
            uri: uri.to_string(),
            track_name: "Retry".to_string(),
            artist_name: "Artist".to_string(),
            cover_url: String::new(),
            spotify_duration_ms: None,
        };

        pending.lock().unwrap().insert(uri.to_string());
        progress
            .lock()
            .unwrap()
            .insert(uri.to_string(), DownloadPhase::Downloading(0.5));
        save_pending_downloads_to(&queue_path, &[request]);

        finish_download_bookkeeping(
            uri,
            &pending,
            &progress,
            &app_state,
            &queue_path,
            PendingQueueAction::KeepForRetry,
        );

        assert!(!pending.lock().unwrap().contains(uri));
        assert!(!progress.lock().unwrap().contains_key(uri));
        let restored = load_pending_downloads_from(&queue_path);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].uri, uri);
        assert!(app_state.lock().unwrap().render_dirty);
    }

    #[test]
    fn pending_download_queue_round_trips_requests() {
        let dir = temp_test_dir("pending-download-roundtrip");
        let path = dir.join("download_queue.json");
        let request = sample_request();

        save_pending_downloads_to(&path, std::slice::from_ref(&request));
        let restored = load_pending_downloads_from(&path);

        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].uri, request.uri);
        assert_eq!(restored[0].track_name, request.track_name);
        assert_eq!(restored[0].artist_name, request.artist_name);
        assert_eq!(restored[0].spotify_duration_ms, request.spotify_duration_ms);
    }

    #[test]
    fn pending_download_queue_removes_terminal_request() {
        let dir = temp_test_dir("pending-download-remove");
        let path = dir.join("download_queue.json");
        let first = sample_request();
        let second = DownloadRequest {
            uri: "spotify:track:456".to_string(),
            track_name: "Second".to_string(),
            artist_name: "Artist".to_string(),
            cover_url: String::new(),
            spotify_duration_ms: None,
        };

        save_pending_downloads_to(&path, &[first.clone(), second.clone()]);
        remove_pending_download_from(&path, &first.uri);

        let restored = load_pending_downloads_from(&path);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].uri, second.uri);
    }

    #[test]
    fn pending_download_queue_can_promote_request_to_front() {
        let dir = temp_test_dir("pending-download-promote");
        let path = dir.join("download_queue.json");
        let first = sample_request();
        let second = DownloadRequest {
            uri: "spotify:track:456".to_string(),
            track_name: "Second".to_string(),
            artist_name: "Artist".to_string(),
            cover_url: String::new(),
            spotify_duration_ms: None,
        };

        save_pending_downloads_to(&path, &[first.clone(), second.clone()]);
        persist_pending_download_to(&path, &second, QueuePlacement::Front);

        let restored = load_pending_downloads_from(&path);
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].uri, second.uri);
        assert_eq!(restored[1].uri, first.uri);
    }

    #[test]
    fn work_queue_promotes_existing_request_to_front() {
        let queue = DownloadWorkQueue::new();
        let first = sample_request();
        let second = DownloadRequest {
            uri: "spotify:track:456".to_string(),
            track_name: "Second".to_string(),
            artist_name: "Artist".to_string(),
            cover_url: String::new(),
            spotify_duration_ms: None,
        };

        queue.enqueue_back(first.clone());
        queue.enqueue_back(second.clone());

        assert!(queue.enqueue_front_promote(second.clone()));

        let queued = queue.snapshot();
        assert_eq!(queued.len(), 2);
        assert_eq!(queued[0].uri, second.uri);
        assert_eq!(queued[1].uri, first.uri);
    }

    #[test]
    fn active_spotify_playback_defers_background_downloads() {
        let mut state = AppState::new();
        state.set_mode(AppMode::Spotify);
        state.set_paused(false);

        assert!(should_defer_download_for_spotify(&state));

        state.set_paused(true);
        assert!(!should_defer_download_for_spotify(&state));

        state.set_mode(AppMode::Local);
        state.set_paused(false);
        assert!(!should_defer_download_for_spotify(&state));
    }

    #[test]
    fn restore_pending_downloads_marks_progress_and_sends_requests() {
        let dir = temp_test_dir("pending-download-restore");
        let path = dir.join("download_queue.json");
        let request = sample_request();
        save_pending_downloads_to(&path, std::slice::from_ref(&request));

        let queue = DownloadWorkQueue::new();
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let progress: DownloadProgressMap = Arc::new(Mutex::new(HashMap::new()));

        let restored = restore_pending_downloads_from(&path, &queue, &pending, &progress);

        assert_eq!(restored, 1);
        assert!(pending.lock().unwrap().contains(&request.uri));
        assert_eq!(
            progress.lock().unwrap().get(&request.uri),
            Some(&DownloadPhase::Queued)
        );
        assert_eq!(queue.snapshot()[0].uri, request.uri);
    }

    #[test]
    fn incomplete_spotify_favorites_are_restored_into_download_queue() {
        let dir = temp_test_dir("incomplete-favorite-restore");
        let queue_path = dir.join("download_queue.json");
        let favorites_path = dir.join("favorites.json");
        let mut favorites = FavoritesManager::load(&favorites_path);
        favorites.add(sample_favorite(
            "spotify:track:missing",
            FavoriteSource::Spotify,
            false,
        ));
        favorites.add(sample_favorite(
            "spotify:track:done",
            FavoriteSource::Spotify,
            true,
        ));
        favorites.add(sample_favorite(
            "local:track:import",
            FavoriteSource::LocalImport,
            false,
        ));
        let favorites = Arc::new(Mutex::new(favorites));
        let queue = DownloadWorkQueue::new();
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let progress: DownloadProgressMap = Arc::new(Mutex::new(HashMap::new()));

        let restored = restore_incomplete_favorite_downloads_from(
            &favorites,
            &queue_path,
            &queue,
            &pending,
            &progress,
        );

        assert_eq!(restored, 1);
        assert!(pending.lock().unwrap().contains("spotify:track:missing"));
        assert_eq!(
            progress.lock().unwrap().get("spotify:track:missing"),
            Some(&DownloadPhase::Queued)
        );

        let queued = load_pending_downloads_from(&queue_path);
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].uri, "spotify:track:missing");
        assert_eq!(queued[0].track_name, "Retry Me");
        assert_eq!(queue.snapshot()[0].uri, "spotify:track:missing");
    }

    fn temp_test_dir(prefix: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "sideb-{prefix}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn classifies_bot_check_cookie_failures() {
        let stderr =
            "ERROR: [youtube] Sign in to confirm you're not a bot. Use --cookies-from-browser";
        assert_eq!(
            classify_download_failure(stderr),
            DownloadFailureKind::CookiesBotCheck
        );
        assert_eq!(
            DownloadFailureKind::CookiesBotCheck.notice(),
            "Cookie check failed"
        );
    }

    #[test]
    fn classifies_signature_challenge_po_token_failures() {
        assert_eq!(
            classify_download_failure("Signature solving failed: some formats may be missing"),
            DownloadFailureKind::SignatureChallenge
        );
        assert_eq!(
            classify_download_failure("web_music client https formats require a GVS PO Token"),
            DownloadFailureKind::SignatureChallenge
        );
        assert_eq!(
            DownloadFailureKind::SignatureChallenge.notice(),
            "YouTube challenge"
        );
    }

    #[test]
    fn classifies_network_dns_tls_timeout_failures() {
        let cases = [
            "Temporary failure in name resolution",
            "Connection timed out",
            "TLS handshake failed",
            "Network is unreachable",
        ];
        for stderr in cases {
            assert_eq!(
                classify_download_failure(stderr),
                DownloadFailureKind::Network
            );
        }
    }

    #[test]
    fn classifies_temp_storage_unpack_failures() {
        let cases = [
            "No space left on device",
            "PyInstaller: failed to extract _brotli to /tmp",
            "could not create temporary file in TMPDIR",
        ];
        for stderr in cases {
            assert_eq!(
                classify_download_failure(stderr),
                DownloadFailureKind::TempStorage
            );
        }
    }

    #[test]
    fn classifies_missing_runtime_helpers() {
        assert_eq!(
            classify_download_failure("failed to run yt-dlp: No such file or directory"),
            DownloadFailureKind::MissingYtDlp
        );
        assert_eq!(
            classify_download_failure(
                "ERROR: ffmpeg not found. Please install or provide --ffmpeg-location"
            ),
            DownloadFailureKind::MissingTranscoder
        );
        assert_eq!(
            classify_download_failure("ERROR: Unknown encoder 'libmp3lame'"),
            DownloadFailureKind::MissingTranscoder
        );
        assert_eq!(
            classify_download_failure("ERROR: muxer s16le is unavailable"),
            DownloadFailureKind::MissingTranscoder
        );
        assert_eq!(DownloadFailureKind::MissingYtDlp.notice(), "Missing yt-dlp");
        assert_eq!(
            DownloadFailureKind::MissingTranscoder.notice(),
            "Audio tool failed"
        );
    }

    #[test]
    fn classifies_no_matching_audio_and_generic_failures() {
        assert_eq!(
            classify_download_failure("Requested format is not available"),
            DownloadFailureKind::NoMatchingAudio
        );
        assert_eq!(
            classify_download_failure("duration mismatch: spotify=1000ms file=5000ms"),
            DownloadFailureKind::NoMatchingAudio
        );
        assert_eq!(
            classify_download_failure("unexpected extractor failure"),
            DownloadFailureKind::Generic
        );
    }

    #[test]
    fn final_download_notice_is_only_shown_for_failed_outcomes() {
        let app_state = Arc::new(Mutex::new(AppState::new()));

        show_final_download_failure_notice(&DownloadOutcome::Skipped, &app_state);
        assert!(app_state.lock().unwrap().notice.is_none());

        show_final_download_failure_notice(&DownloadOutcome::Success, &app_state);
        assert!(app_state.lock().unwrap().notice.is_none());

        show_final_download_failure_notice(
            &DownloadOutcome::Failed(DownloadFailureKind::Network),
            &app_state,
        );

        assert_eq!(
            app_state
                .lock()
                .unwrap()
                .active_notice_message(std::time::Instant::now()),
            Some("Network error".to_string())
        );
    }
}
