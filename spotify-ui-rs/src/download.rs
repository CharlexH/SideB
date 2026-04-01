use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::constants::{FFMPEG_TRANSCODER_BIN, YTDLP_BIN};
use crate::favorites::FavoritesManager;
use crate::log_utils::{exit_status_label, format_bytes, summarize_command_output};
use crate::paths::app_paths;

const MAX_RETRIES: u32 = 1;
const RETRY_DELAY_SECS: u64 = 3;

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
    pending_uris: Arc<Mutex<HashSet<String>>>,
}

impl DownloadManager {
    /// Create a new manager and spawn the background download thread.
    pub fn new(favorites: Arc<Mutex<FavoritesManager>>) -> Self {
        let (tx, rx) = mpsc::channel::<DownloadRequest>();
        let pending_uris = Arc::new(Mutex::new(HashSet::new()));
        let pending_clone = Arc::clone(&pending_uris);

        std::thread::Builder::new()
            .name("download".into())
            .spawn(move || {
                download_loop(rx, favorites, pending_clone);
            })
            .expect("spawn download thread");

        Self { tx, pending_uris }
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
        if let Err(e) = self.tx.send(request) {
            eprintln!("download: enqueue failed: {e}");
        } else {
            eprintln!(
                "download: queued uri={} track={} - {} pending={}",
                uri, artist_name, track_name, pending_count
            );
        }
    }
}

fn build_search_query(request: &DownloadRequest) -> String {
    format!("{} - {}", request.artist_name, request.track_name)
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
fn download_loop(
    rx: mpsc::Receiver<DownloadRequest>,
    favorites: Arc<Mutex<FavoritesManager>>,
    pending_uris: Arc<Mutex<HashSet<String>>>,
) {
    for req in rx.iter() {
        eprintln!(
            "download: starting uri={} track={} - {}",
            req.uri, req.artist_name, req.track_name
        );

        {
            let fav = favorites.lock().unwrap();
            if !fav.is_favorited(&req.uri) {
                eprintln!("download: skipping (unfavorited): {}", req.uri);
                pending_uris.lock().unwrap().remove(&req.uri);
                continue;
            }
            if fav.find_by_uri(&req.uri).map_or(false, |e| e.downloaded) {
                eprintln!("download: skipping (already downloaded): {}", req.uri);
                pending_uris.lock().unwrap().remove(&req.uri);
                continue;
            }
        }

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

        if output_path.exists() {
            let is_downloaded = favorites
                .lock()
                .unwrap()
                .find_by_uri(&req.uri)
                .map_or(false, |e| e.downloaded);
            if !is_downloaded {
                eprintln!(
                    "download: removing stale partial file: {}",
                    output_path.display()
                );
                let _ = std::fs::remove_file(&output_path);
            }
        }

        let search_query = build_search_query(&req);
        let output_template = output_path.to_string_lossy().to_string();
        let mut success = false;

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                eprintln!(
                    "download: retry {attempt}/{MAX_RETRIES} for {} - {}",
                    req.artist_name, req.track_name
                );
                std::thread::sleep(std::time::Duration::from_secs(RETRY_DELAY_SECS));

                if !favorites.lock().unwrap().is_favorited(&req.uri) {
                    eprintln!("download: skipping retry (unfavorited): {}", req.uri);
                    break;
                }
            }

            eprintln!(
                "download: launching yt-dlp attempt={}/{} uri={} query={} output={} ytdlp={} ffmpeg={}",
                attempt + 1,
                MAX_RETRIES + 1,
                req.uri,
                search_query,
                output_path.display(),
                YTDLP_BIN,
                FFMPEG_TRANSCODER_BIN
            );
            let result = Command::new(YTDLP_BIN)
                .args([
                    "-x",
                    "--audio-format",
                    "mp3",
                    "--audio-quality",
                    "5",
                    "--no-playlist",
                    "--ffmpeg-location",
                    FFMPEG_TRANSCODER_BIN,
                    "-o",
                    &output_template,
                    &format!("ytsearch1:{}", search_query),
                ])
                .output();

            match result {
                Ok(output) if output.status.success() => {
                    success = true;
                    break;
                }
                Ok(output) => {
                    eprintln!(
                        "download: yt-dlp failed attempt={}/{} uri={} status={} stderr={}",
                        attempt + 1,
                        MAX_RETRIES + 1,
                        req.uri,
                        exit_status_label(&output.status),
                        summarize_command_output(&output.stderr)
                    );
                    if output_path.exists() {
                        let _ = std::fs::remove_file(&output_path);
                    }
                }
                Err(e) => {
                    eprintln!("download: failed to run yt-dlp: {e}");
                    break;
                }
            }
        }

        if success {
            let size = fs::metadata(&output_path)
                .map(|meta| format_bytes(meta.len()))
                .unwrap_or_else(|_| "unknown".to_string());
            eprintln!(
                "download: success uri={} mp3={} size={}",
                req.uri,
                output_path.display(),
                size
            );

            let duration_ms = probe_duration(&output_path);

            let mut fav = favorites.lock().unwrap();
            fav.mark_downloaded(&req.uri, &output_path.to_string_lossy(), duration_ms);
            eprintln!(
                "download: library updated uri={} duration_ms={:?}",
                req.uri, duration_ms
            );

            let cover_downloaded = download_cover(&req.cover_url, &cover_path);
            if !cover_path.exists() && !req.cover_url.is_empty() {
                let copied = try_copy_from_cover_cache(&req.cover_url, &cover_path);
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
        } else {
            if output_path.exists() {
                let _ = std::fs::remove_file(&output_path);
                eprintln!(
                    "download: removed failed partial uri={} path={}",
                    req.uri,
                    output_path.display()
                );
            }
            eprintln!(
                "download: giving up uri={} track={} - {}",
                req.uri, req.artist_name, req.track_name
            );
        }

        pending_uris.lock().unwrap().remove(&req.uri);
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
            eprintln!("download: ffprobe launch failed for {}: {e}", path.display());
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

    fn sample_request() -> DownloadRequest {
        DownloadRequest {
            uri: "spotify:track:123".to_string(),
            track_name: "Komm, Susser Tod".to_string(),
            artist_name: "Arianne".to_string(),
            cover_url: String::new(),
        }
    }

    #[test]
    fn legacy_download_query_uses_artist_dash_track_format() {
        let request = sample_request();

        assert_eq!(build_search_query(&request), "Arianne - Komm, Susser Tod");
    }
}
