use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::constants::{FFMPEG_FULL_BIN, MUSIC_DIR, YTDLP_BIN};
use crate::favorites::FavoritesManager;

/// A request to download a track in the background.
#[derive(Debug)]
pub struct DownloadRequest {
    pub uri: String,
    pub track_name: String,
    pub artist_name: String,
    pub cover_url: String,
}

/// Manages a queue of background downloads via yt-dlp.
pub struct DownloadManager {
    tx: mpsc::Sender<DownloadRequest>,
}

impl DownloadManager {
    /// Create a new manager and spawn the background download thread.
    pub fn new(favorites: Arc<Mutex<FavoritesManager>>) -> Self {
        let (tx, rx) = mpsc::channel::<DownloadRequest>();

        std::thread::Builder::new()
            .name("download".into())
            .spawn(move || {
                download_loop(rx, favorites);
            })
            .expect("spawn download thread");

        Self { tx }
    }

    /// Queue a download request. Non-blocking.
    pub fn enqueue(&self, request: DownloadRequest) {
        if let Err(e) = self.tx.send(request) {
            eprintln!("download: enqueue failed: {e}");
        }
    }
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

/// Background loop that processes download requests one at a time.
fn download_loop(rx: mpsc::Receiver<DownloadRequest>, favorites: Arc<Mutex<FavoritesManager>>) {
    for req in rx.iter() {
        eprintln!(
            "download: starting {} - {}",
            req.artist_name, req.track_name
        );

        // Check if still favorited (user may have unfavorited while queued)
        {
            let fav = favorites.lock().unwrap();
            if !fav.is_favorited(&req.uri) {
                eprintln!("download: skipping (unfavorited): {}", req.uri);
                continue;
            }
        }

        // Ensure music directory exists
        let _ = std::fs::create_dir_all(MUSIC_DIR);

        let safe_artist = sanitize_filename(&req.artist_name);
        let safe_track = sanitize_filename(&req.track_name);
        let base_name = format!("{} - {}", safe_artist, safe_track);
        let output_path = PathBuf::from(MUSIC_DIR).join(format!("{}.mp3", base_name));
        let cover_path = PathBuf::from(MUSIC_DIR).join(format!("{}.jpg", base_name));

        // Download via yt-dlp
        let search_query = format!("{} - {}", req.artist_name, req.track_name);
        let output_template = output_path.to_string_lossy().to_string();

        let result = Command::new(YTDLP_BIN)
            .args([
                "-x",
                "--audio-format",
                "mp3",
                "--audio-quality",
                "5", // reasonable quality, smaller file
                "--no-playlist",
                "--no-overwrites",
                "--ffmpeg-location",
                FFMPEG_FULL_BIN,
                "-o",
                &output_template,
                &format!("ytsearch1:{}", search_query),
            ])
            .output();

        match result {
            Ok(output) => {
                if output.status.success() {
                    eprintln!("download: success: {}", output_path.display());

                    // Get duration via ffprobe
                    let duration_ms = probe_duration(&output_path);

                    // Update favorites
                    let mut fav = favorites.lock().unwrap();
                    fav.mark_downloaded(
                        &req.uri,
                        &output_path.to_string_lossy(),
                        duration_ms,
                    );

                    // Try to save cover art (download via curl)
                    download_cover(&req.cover_url, &cover_path);
                    // If curl failed, try copying from Spotify cover cache
                    if !cover_path.exists() && !req.cover_url.is_empty() {
                        try_copy_from_cover_cache(&req.cover_url, &cover_path);
                    }
                    if cover_path.exists() {
                        fav.set_cover_path(&req.uri, &cover_path.to_string_lossy());
                    }
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    eprintln!("download: yt-dlp failed: {}", stderr.lines().last().unwrap_or(""));
                }
            }
            Err(e) => {
                eprintln!("download: failed to run yt-dlp: {e}");
            }
        }
    }
}

/// Use ffprobe to get track duration in milliseconds.
fn probe_duration(path: &Path) -> Option<i64> {
    let output = Command::new("ffprobe")
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
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let s = String::from_utf8_lossy(&output.stdout);
    let secs: f64 = s.trim().parse().ok()?;
    Some((secs * 1000.0) as i64)
}

/// Download cover art via curl (HTTPS support).
fn download_cover(url: &str, dest: &Path) {
    if url.is_empty() {
        return;
    }

    // Use the existing cert file path
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
                eprintln!("download: cover fetch failed for {}", url);
            }
        }
        Err(e) => {
            eprintln!("download: curl error: {e}");
        }
    }
}

/// Try to copy cover art from Spotify's local cover cache.
/// The cache stores original JPEG bytes keyed by FNV hash of the URL.
fn try_copy_from_cover_cache(url: &str, dest: &Path) {
    // Replicate the same FNV hash used in network.rs
    let mut hash = 0xcbf29ce484222325u64;
    for &byte in url.as_bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let cache_path = PathBuf::from("/tmp/spotify-ui-cover-cache").join(format!("{hash:016x}.img"));

    if cache_path.exists() {
        match std::fs::copy(&cache_path, dest) {
            Ok(_) => eprintln!("download: cover copied from cache {}", cache_path.display()),
            Err(e) => eprintln!("download: cache copy failed: {e}"),
        }
    }
}
