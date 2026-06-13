use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::constants::SYSTEM_FFMPEG_BIN;
use crate::favorites::{FavoriteEntry, FavoriteSource, FavoritesManager};
use crate::mode::InputAction;
use crate::paths::app_paths;
use crate::resources;

const IMPORT_SCAN_INTERVAL: Duration = Duration::from_secs(3);
const MIN_IMPORT_PROGRESS_VISIBLE: Duration = Duration::from_millis(1500);
const ITUNES_SEARCH_BASE_URL: &str = "https://itunes.apple.com/search";
static EXISTING_ONLINE_COVER_ATTEMPTS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImportMetadata {
    title: String,
    artist: String,
    album: String,
    duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetadataSource {
    Ffprobe,
    Filename,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImportScanReport {
    candidates: usize,
    imported: usize,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    tags: Option<FfprobeTags>,
}

#[derive(Debug, Deserialize)]
struct FfprobeTags {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    album: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    format: Option<FfprobeFormat>,
}

pub fn scan_once(favorites: &Arc<Mutex<FavoritesManager>>) -> usize {
    scan_once_report(favorites, None).imported
}

pub fn pending_import_count() -> usize {
    let imports_dir = app_paths().imports_dir.clone();
    let _ = fs::create_dir_all(&imports_dir);
    collect_import_candidates(&imports_dir).len()
}

fn scan_once_report(
    favorites: &Arc<Mutex<FavoritesManager>>,
    cmd_tx: Option<&Sender<InputAction>>,
) -> ImportScanReport {
    let imports_dir = app_paths().imports_dir.clone();
    let music_dir = app_paths().music_dir.clone();
    let _ = fs::create_dir_all(&imports_dir);
    let _ = fs::create_dir_all(&music_dir);

    let import_candidates = collect_import_candidates(&imports_dir);

    if !import_candidates.is_empty() {
        eprintln!(
            "import: found {} candidate mp3 file(s) in {}",
            import_candidates.len(),
            imports_dir.display()
        );
        if let Some(tx) = cmd_tx {
            let _ = tx.send(InputAction::ImportProgress {
                completed: 0,
                total: import_candidates.len(),
            });
        }
    }

    let mut imported = 0usize;
    let candidates = import_candidates.len();
    for (idx, path) in import_candidates.into_iter().enumerate() {
        let allow_directory_cover = path
            .parent()
            .map(|parent| parent != imports_dir.as_path())
            .unwrap_or(false);
        match import_one(&path, &music_dir, allow_directory_cover) {
            Ok(entry) => {
                favorites.lock().unwrap().add(entry);
                imported += 1;
            }
            Err(e) => {
                eprintln!("import: {}: {e}", path.display());
            }
        }
        if let Some(tx) = cmd_tx {
            let _ = tx.send(InputAction::ImportProgress {
                completed: idx + 1,
                total: candidates,
            });
        }
    }

    ImportScanReport {
        candidates,
        imported,
    }
}

pub fn sync_existing_music_covers(favorites: &Arc<Mutex<FavoritesManager>>) -> usize {
    let entries = {
        let fav = favorites.lock().unwrap();
        fav.downloaded_entries()
    };

    let mut updated = 0usize;
    for entry in entries {
        let needs_cover = match entry.cover_path.as_deref() {
            Some(path) => !Path::new(path).exists(),
            None => true,
        };
        if !needs_cover {
            continue;
        }

        let Some(file_path) = entry.file_path.as_deref() else {
            continue;
        };
        let audio_path = Path::new(file_path);
        let cover_path = if let Some(cover_path) = find_sidecar_cover(audio_path) {
            eprintln!(
                "import: linked existing sidecar cover {} -> {}",
                cover_path.display(),
                entry.uri
            );
            cover_path
        } else {
            let target_cover = audio_path.with_extension("jpg");
            if !should_attempt_existing_online_cover(&entry.uri) {
                continue;
            }
            if fetch_existing_online_cover(&entry, &target_cover) {
                eprintln!(
                    "import: backfilled existing cover {} -> {}",
                    target_cover.display(),
                    entry.uri
                );
                target_cover
            } else {
                eprintln!(
                    "import: existing cover backfill unavailable uri={}",
                    entry.uri
                );
                continue;
            }
        };

        favorites
            .lock()
            .unwrap()
            .set_cover_path(&entry.uri, &cover_path.to_string_lossy());
        updated += 1;
    }

    updated
}

fn collect_import_candidates(imports_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    collect_import_candidates_into(imports_dir, &mut candidates);
    candidates.sort();
    candidates
}

fn collect_import_candidates_into(dir: &Path, candidates: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("import: read_dir failed for {}: {e}", dir.display());
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(e) => {
                eprintln!("import: file_type failed for {}: {e}", path.display());
                continue;
            }
        };

        if file_type.is_dir() {
            if should_skip_import_dir(&path) {
                continue;
            }
            collect_import_candidates_into(&path, candidates);
        } else if file_type.is_file() && is_importable_mp3_path(&path) {
            candidates.push(path);
        }
    }
}

fn should_skip_import_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name == "__MACOSX" || name.starts_with('.'))
        .unwrap_or(false)
}

fn is_importable_mp3_path(path: &Path) -> bool {
    is_mp3_path(path) && !is_appledouble_file(path)
}

fn is_mp3_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("mp3"))
        .unwrap_or(false)
}

fn is_appledouble_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with("._"))
        .unwrap_or(false)
}

pub fn run(
    favorites: Arc<Mutex<FavoritesManager>>,
    cmd_tx: Sender<InputAction>,
    quit: Arc<AtomicBool>,
) {
    let _ = fs::create_dir_all(&app_paths().imports_dir);
    while !quit.load(Ordering::Relaxed) {
        let scan_started = Instant::now();
        let report = scan_once_report(&favorites, Some(&cmd_tx));
        let cover_updates = sync_existing_music_covers(&favorites);
        if report.imported > 0 {
            eprintln!("import: added {} local track(s)", report.imported);
        }
        if cover_updates > 0 {
            eprintln!("import: linked {cover_updates} existing cover file(s)");
        }
        if report.candidates > 0 {
            let elapsed = scan_started.elapsed();
            if elapsed < MIN_IMPORT_PROGRESS_VISIBLE {
                std::thread::sleep(MIN_IMPORT_PROGRESS_VISIBLE - elapsed);
            }
            let _ = cmd_tx.send(InputAction::ImportFinished);
        }
        if report.imported > 0 || cover_updates > 0 {
            let _ = cmd_tx.send(InputAction::LibraryChanged);
        }

        for _ in 0..IMPORT_SCAN_INTERVAL.as_secs() {
            if quit.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

fn import_one(
    import_mp3: &Path,
    music_dir: &Path,
    allow_directory_cover: bool,
) -> Result<FavoriteEntry, String> {
    eprintln!("import: processing {}", import_mp3.display());
    let source_stem = import_mp3
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Imported Track");
    let (metadata, metadata_source) = resolve_metadata(import_mp3, source_stem);
    eprintln!(
        "import: metadata source={} artist={} title={} duration_ms={:?}",
        metadata_source.label(),
        metadata.artist,
        metadata.title,
        metadata.duration_ms
    );

    let base_name = sanitize_filename(&format!("{} - {}", metadata.artist, metadata.title));
    let target_mp3 = unique_target_path(music_dir, &base_name, "mp3");
    fs::rename(import_mp3, &target_mp3)
        .map_err(|e| format!("move to {} failed: {e}", target_mp3.display()))?;
    eprintln!(
        "import: moved {} -> {}",
        import_mp3.display(),
        target_mp3.display()
    );

    let import_sidecar = find_sidecar_cover(import_mp3);
    let import_directory_cover = allow_directory_cover
        .then(|| find_import_directory_cover(import_mp3))
        .flatten();
    let embedded_cover_target = target_mp3.with_extension("jpg");
    let used_embedded_cover = extract_embedded_cover(&target_mp3, &embedded_cover_target);
    let cover_path = if used_embedded_cover {
        eprintln!(
            "import: embedded cover extracted to {}",
            embedded_cover_target.display()
        );
        Some(embedded_cover_target)
    } else if let Some(sidecar) = import_sidecar.as_ref() {
        let ext = sidecar
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("jpg");
        let target_cover = unique_target_path(music_dir, &base_name, ext);
        match fs::copy(sidecar, &target_cover) {
            Ok(_) => {
                eprintln!(
                    "import: sidecar cover copied {} -> {}",
                    sidecar.display(),
                    target_cover.display()
                );
                Some(target_cover)
            }
            Err(e) => {
                eprintln!(
                    "import: copy cover {} -> {} failed: {e}",
                    sidecar.display(),
                    target_cover.display()
                );
                None
            }
        }
    } else if let Some(directory_cover) = import_directory_cover.as_ref() {
        let ext = directory_cover
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("jpg");
        let target_cover = unique_target_path(music_dir, &base_name, ext);
        match fs::copy(directory_cover, &target_cover) {
            Ok(_) => {
                eprintln!(
                    "import: directory cover copied {} -> {}",
                    directory_cover.display(),
                    target_cover.display()
                );
                Some(target_cover)
            }
            Err(e) => {
                eprintln!(
                    "import: copy cover {} -> {} failed: {e}",
                    directory_cover.display(),
                    target_cover.display()
                );
                None
            }
        }
    } else {
        let online_cover_target = target_mp3.with_extension("jpg");
        if fetch_itunes_cover(&metadata, &online_cover_target) {
            eprintln!(
                "import: online cover fetched to {}",
                online_cover_target.display()
            );
            Some(online_cover_target)
        } else {
            eprintln!("import: no cover found for {}", target_mp3.display());
            None
        }
    };

    if let Some(sidecar) = import_sidecar {
        if used_embedded_cover || cover_path.is_some() {
            let _ = fs::remove_file(sidecar);
        }
    }

    let file_name = target_mp3
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "target file name missing".to_string())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let entry = FavoriteEntry {
        uri: format!("local:{file_name}"),
        name: metadata.title,
        artist: metadata.artist,
        album: metadata.album,
        cover_url: String::new(),
        source: FavoriteSource::LocalImport,
        file_path: Some(target_mp3.to_string_lossy().to_string()),
        cover_path: cover_path.map(|path| path.to_string_lossy().to_string()),
        duration_ms: metadata.duration_ms,
        spotify_duration_ms: None,
        downloaded: true,
        added_at: now.to_string(),
    };
    eprintln!(
        "import: ready uri={} file={} cover={}",
        entry.uri,
        entry.file_path.as_deref().unwrap_or("none"),
        entry.cover_path.as_deref().unwrap_or("none")
    );

    Ok(entry)
}

fn probe_metadata(path: &Path) -> Option<ImportMetadata> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_entries",
            "format=duration:format_tags=title,artist,album",
        ])
        .arg(path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    parse_ffprobe_metadata(
        &String::from_utf8_lossy(&output.stdout),
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Imported Track"),
    )
}

fn resolve_metadata(path: &Path, fallback_stem: &str) -> (ImportMetadata, MetadataSource) {
    match probe_metadata(path) {
        Some(metadata) => (metadata, MetadataSource::Ffprobe),
        None => (
            metadata_from_filename(fallback_stem),
            MetadataSource::Filename,
        ),
    }
}

fn parse_ffprobe_metadata(json: &str, fallback_stem: &str) -> Option<ImportMetadata> {
    let parsed: FfprobeOutput = serde_json::from_str(json).ok()?;
    let fallback = metadata_from_filename(fallback_stem);
    let format = parsed.format?;
    let tags = format.tags.unwrap_or(FfprobeTags {
        title: None,
        artist: None,
        album: None,
    });

    let title = tags
        .title
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(fallback.title);
    let artist = tags
        .artist
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(fallback.artist);
    let album = tags
        .album
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(fallback.album);
    let duration_ms = format
        .duration
        .and_then(|s| s.parse::<f64>().ok())
        .map(|secs| (secs * 1000.0) as i64);

    Some(ImportMetadata {
        title,
        artist,
        album,
        duration_ms,
    })
}

fn metadata_from_filename(stem: &str) -> ImportMetadata {
    let trimmed = stem.trim();
    if let Some((artist, title)) = trimmed.split_once(" - ") {
        ImportMetadata {
            title: title.trim().to_string(),
            artist: artist.trim().to_string(),
            album: String::new(),
            duration_ms: None,
        }
    } else {
        ImportMetadata {
            title: trimmed.to_string(),
            artist: "Unknown Artist".to_string(),
            album: String::new(),
            duration_ms: None,
        }
    }
}

fn sanitize_filename(s: &str) -> String {
    let sanitized = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_string();
    if sanitized.is_empty() {
        "Imported Track".to_string()
    } else {
        sanitized
    }
}

fn unique_target_path(dir: &Path, base_name: &str, ext: &str) -> PathBuf {
    let mut candidate = dir.join(format!("{base_name}.{ext}"));
    let mut suffix = 2usize;
    while candidate.exists() {
        candidate = dir.join(format!("{base_name} ({suffix}).{ext}"));
        suffix += 1;
    }
    candidate
}

fn find_sidecar_cover(import_mp3: &Path) -> Option<PathBuf> {
    let stem = import_mp3.file_stem()?.to_str()?;
    let parent = import_mp3.parent()?;
    let mut entries = fs::read_dir(parent)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();

    for preferred_ext in ["jpg", "jpeg", "png"] {
        for candidate in &entries {
            if !candidate.is_file() {
                continue;
            }
            let Some(candidate_stem) = candidate.file_stem().and_then(|value| value.to_str())
            else {
                continue;
            };
            let Some(candidate_ext) = candidate.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            if candidate_stem == stem && candidate_ext.eq_ignore_ascii_case(preferred_ext) {
                return Some(candidate.clone());
            }
        }
    }
    None
}

fn find_import_directory_cover(import_mp3: &Path) -> Option<PathBuf> {
    let parent = import_mp3.parent()?;
    let mut entries = fs::read_dir(parent)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();

    for preferred_stem in ["cover", "folder"] {
        for preferred_ext in ["jpg", "jpeg", "png"] {
            for candidate in &entries {
                if !candidate.is_file() {
                    continue;
                }
                let Some(candidate_stem) = candidate.file_stem().and_then(|value| value.to_str())
                else {
                    continue;
                };
                let Some(candidate_ext) = candidate.extension().and_then(|value| value.to_str())
                else {
                    continue;
                };
                if candidate_stem.eq_ignore_ascii_case(preferred_stem)
                    && candidate_ext.eq_ignore_ascii_case(preferred_ext)
                {
                    return Some(candidate.clone());
                }
            }
        }
    }
    None
}

fn extract_embedded_cover(audio_path: &Path, dest: &Path) -> bool {
    let output = Command::new(embedded_cover_extractor_bin())
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(audio_path)
        .args(["-an", "-map", "0:v:0", "-frames:v", "1"])
        .arg(dest)
        .output();

    match output {
        Ok(result) if result.status.success() && dest.exists() => true,
        _ => {
            let _ = fs::remove_file(dest);
            false
        }
    }
}

fn embedded_cover_extractor_bin() -> &'static str {
    SYSTEM_FFMPEG_BIN
}

fn fetch_itunes_cover(metadata: &ImportMetadata, dest: &Path) -> bool {
    if cfg!(test) && std::env::var_os("SIDEB_TEST_ENABLE_ONLINE_COVER").is_none() {
        return false;
    }

    if metadata.title.trim().is_empty()
        || metadata
            .artist
            .trim()
            .eq_ignore_ascii_case("unknown artist")
    {
        return false;
    }

    let search_url = build_itunes_search_url(metadata);
    let json = match fetch_text_url(&search_url) {
        Some(json) => json,
        None => return false,
    };
    let artwork_url = match select_itunes_artwork_url(&json, metadata) {
        Some(url) => url,
        None => return false,
    };

    download_url_to_file(&artwork_url, dest)
}

fn fetch_existing_online_cover(entry: &FavoriteEntry, dest: &Path) -> bool {
    if cfg!(test) && std::env::var_os("SIDEB_TEST_ENABLE_ONLINE_COVER").is_none() {
        return false;
    }

    match existing_online_cover_source(entry) {
        Some(ExistingOnlineCoverSource::DirectUrl(url)) => download_url_to_file(&url, dest),
        Some(ExistingOnlineCoverSource::Itunes(metadata)) => fetch_itunes_cover(&metadata, dest),
        None => false,
    }
}

fn should_attempt_existing_online_cover(uri: &str) -> bool {
    let attempts = EXISTING_ONLINE_COVER_ATTEMPTS.get_or_init(|| Mutex::new(HashSet::new()));
    attempts.lock().unwrap().insert(uri.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExistingOnlineCoverSource {
    DirectUrl(String),
    Itunes(ImportMetadata),
}

fn existing_online_cover_source(entry: &FavoriteEntry) -> Option<ExistingOnlineCoverSource> {
    match entry.source {
        FavoriteSource::Spotify if !entry.cover_url.trim().is_empty() => Some(
            ExistingOnlineCoverSource::DirectUrl(entry.cover_url.clone()),
        ),
        FavoriteSource::LocalImport => Some(ExistingOnlineCoverSource::Itunes(ImportMetadata {
            title: entry.name.clone(),
            artist: entry.artist.clone(),
            album: entry.album.clone(),
            duration_ms: entry.duration_ms,
        })),
        _ => None,
    }
}

fn build_itunes_search_url(metadata: &ImportMetadata) -> String {
    let query = [
        metadata.artist.as_str(),
        metadata.title.as_str(),
        metadata.album.as_str(),
    ]
    .into_iter()
    .filter(|part| !part.trim().is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    format!(
        "{ITUNES_SEARCH_BASE_URL}?media=music&entity=song&limit=5&term={}",
        url_encode_query(&query)
    )
}

fn select_itunes_artwork_url(json: &str, metadata: &ImportMetadata) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let results = value.get("results")?.as_array()?;

    results
        .iter()
        .filter_map(|result| {
            let url = result.get("artworkUrl100")?.as_str()?;
            let artist = result
                .get("artistName")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let track = result
                .get("trackName")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let album = result
                .get("collectionName")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let score = artwork_match_score(metadata, artist, track, album);
            Some((score, upscale_itunes_artwork_url(url)))
        })
        .max_by(|(left_score, _), (right_score, _)| left_score.cmp(right_score))
        .and_then(|(score, url)| (score > 0).then_some(url))
}

fn artwork_match_score(metadata: &ImportMetadata, artist: &str, track: &str, album: &str) -> u8 {
    let mut score = 0;
    if normalized_eq(artist, &metadata.artist) {
        score += 4;
    }
    if normalized_eq(track, &metadata.title) {
        score += 4;
    }
    if !metadata.album.trim().is_empty() && normalized_eq(album, &metadata.album) {
        score += 2;
    }
    score
}

fn normalized_eq(left: &str, right: &str) -> bool {
    normalize_match_text(left) == normalize_match_text(right)
}

fn normalize_match_text(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn upscale_itunes_artwork_url(url: &str) -> String {
    url.replace("100x100bb", "600x600bb")
        .replace("100x100-75", "600x600-75")
}

fn fetch_text_url(url: &str) -> Option<String> {
    let output = curl_base_command(url).output().ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn download_url_to_file(url: &str, dest: &Path) -> bool {
    let mut cmd = curl_base_command(url);
    cmd.args(["-o"]).arg(dest);

    match cmd.output() {
        Ok(output) if output.status.success() && dest.exists() => true,
        _ => {
            let _ = fs::remove_file(dest);
            false
        }
    }
}

fn curl_base_command(url: &str) -> Command {
    let mut cmd = Command::new("curl");
    cmd.args(["-4", "-fsSL", "--connect-timeout", "5", "--max-time", "15"]);
    if let Some(cert_file) = resources::find_resource("ca-certificates.crt") {
        cmd.args(["--cacert", &cert_file.to_string_lossy()]);
    } else if let Ok(cert_file) = std::env::var("SSL_CERT_FILE") {
        cmd.args(["--cacert", &cert_file]);
    }
    cmd.arg(url);
    cmd
}

fn url_encode_query(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char)
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

impl MetadataSource {
    fn label(self) -> &'static str {
        match self {
            Self::Ffprobe => "ffprobe",
            Self::Filename => "filename",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_falls_back_to_filename_when_tags_missing() {
        let meta = metadata_from_filename("Utada Hikaru - Sakura Nagashi");
        assert_eq!(meta.artist, "Utada Hikaru");
        assert_eq!(meta.title, "Sakura Nagashi");
        assert_eq!(meta.album, "");
        assert_eq!(meta.duration_ms, None);
    }

    #[test]
    fn ffprobe_json_prefers_tags_but_keeps_duration() {
        let json = r#"{
          "format": {
            "duration": "12.345",
            "tags": { "title": "Track", "artist": "Artist", "album": "Album" }
          }
        }"#;
        let meta = parse_ffprobe_metadata(json, "Fallback - Name").unwrap();
        assert_eq!(meta.title, "Track");
        assert_eq!(meta.artist, "Artist");
        assert_eq!(meta.album, "Album");
        assert_eq!(meta.duration_ms, Some(12345));
    }

    #[test]
    fn unique_target_path_adds_numeric_suffix() {
        let base = std::env::temp_dir().join(format!(
            "sideb-import-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        let existing = base.join("Artist - Song.mp3");
        fs::write(&existing, b"test").unwrap();

        let next = unique_target_path(&base, "Artist - Song", "mp3");
        assert_eq!(
            next.file_name().and_then(|name| name.to_str()),
            Some("Artist - Song (2).mp3")
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn import_candidates_include_nested_mp3_files() {
        let base = std::env::temp_dir().join(format!(
            "sideb-import-recursive-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let disc_one = base.join("Disc 1");
        let deep = base.join("Disc 2").join("Deep");
        fs::create_dir_all(&disc_one).unwrap();
        fs::create_dir_all(&deep).unwrap();
        fs::write(base.join("Root.mp3"), b"mp3").unwrap();
        fs::write(disc_one.join("Nested.mp3"), b"mp3").unwrap();
        fs::write(deep.join("Upper.MP3"), b"mp3").unwrap();
        fs::write(deep.join("notes.txt"), b"text").unwrap();

        let candidates = collect_import_candidates(&base);
        let relative: Vec<String> = candidates
            .iter()
            .map(|path| {
                path.strip_prefix(&base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();

        assert_eq!(
            relative,
            vec!["Disc 1/Nested.mp3", "Disc 2/Deep/Upper.MP3", "Root.mp3"]
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn import_candidates_skip_macos_hidden_dirs_and_appledouble_mp3_files() {
        let base = std::env::temp_dir().join(format!(
            "sideb-import-filter-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let album = base.join("Album");
        let hidden = base.join(".hidden");
        let macosx = base.join("__MACOSX");
        fs::create_dir_all(&album).unwrap();
        fs::create_dir_all(&hidden).unwrap();
        fs::create_dir_all(&macosx).unwrap();
        fs::write(album.join("Nested.mp3"), b"mp3").unwrap();
        fs::write(album.join("._Nested.mp3"), b"appledouble").unwrap();
        fs::write(hidden.join("Hidden.mp3"), b"mp3").unwrap();
        fs::write(macosx.join("ResourceFork.mp3"), b"mp3").unwrap();

        let candidates = collect_import_candidates(&base);
        let relative: Vec<String> = candidates
            .iter()
            .map(|path| {
                path.strip_prefix(&base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();

        assert_eq!(relative, vec!["Album/Nested.mp3"]);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn sidecar_cover_match_is_case_insensitive() {
        let base = std::env::temp_dir().join(format!(
            "sideb-cover-case-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        let mp3 = base.join("Artist - Song.mp3");
        let cover = base.join("Artist - Song.JPG");
        fs::write(&mp3, b"mp3").unwrap();
        fs::write(&cover, b"jpg").unwrap();

        assert_eq!(find_sidecar_cover(&mp3), Some(cover));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn import_one_uses_album_cover_after_same_name_sidecar_and_keeps_source() {
        let base = std::env::temp_dir().join(format!(
            "sideb-album-cover-import-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let imports_album = base.join("imports").join("Album");
        let music_dir = base.join("music");
        fs::create_dir_all(&imports_album).unwrap();
        fs::create_dir_all(&music_dir).unwrap();
        let first_mp3 = imports_album.join("Artist - First.mp3");
        let second_mp3 = imports_album.join("Artist - Second.mp3");
        let sidecar = imports_album.join("Artist - First.png");
        let album_cover = imports_album.join("cover.jpg");
        fs::write(&first_mp3, b"mp3").unwrap();
        fs::write(&second_mp3, b"mp3").unwrap();
        fs::write(&sidecar, b"sidecar").unwrap();
        fs::write(&album_cover, b"album-cover").unwrap();

        let first = import_one(&first_mp3, &music_dir, true).unwrap();
        let second = import_one(&second_mp3, &music_dir, true).unwrap();
        let first_cover = PathBuf::from(first.cover_path.unwrap());
        let second_cover = PathBuf::from(second.cover_path.unwrap());

        assert_eq!(fs::read(&first_cover).unwrap(), b"sidecar");
        assert_eq!(fs::read(&second_cover).unwrap(), b"album-cover");
        assert!(!sidecar.exists());
        assert!(album_cover.exists());
        assert_ne!(first_cover, album_cover);
        assert_ne!(second_cover, album_cover);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn import_one_does_not_use_root_import_directory_cover() {
        let base = std::env::temp_dir().join(format!(
            "sideb-root-cover-import-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let imports_dir = base.join("imports");
        let music_dir = base.join("music");
        fs::create_dir_all(&imports_dir).unwrap();
        fs::create_dir_all(&music_dir).unwrap();
        let mp3 = imports_dir.join("Artist - Root.mp3");
        let root_cover = imports_dir.join("cover.jpg");
        fs::write(&mp3, b"mp3").unwrap();
        fs::write(&root_cover, b"root-cover").unwrap();

        let entry = import_one(&mp3, &music_dir, false).unwrap();

        assert_eq!(entry.cover_path, None);
        assert!(root_cover.exists());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn sync_existing_music_covers_does_not_link_global_cover_file() {
        let base = std::env::temp_dir().join(format!(
            "sideb-global-cover-sync-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let music_dir = base.join("music");
        fs::create_dir_all(&music_dir).unwrap();
        let mp3 = music_dir.join("Artist - Song.mp3");
        let cover = music_dir.join("cover.jpg");
        fs::write(&mp3, b"mp3").unwrap();
        fs::write(&cover, b"jpg").unwrap();

        let favorites = Arc::new(Mutex::new(FavoritesManager::load(
            base.join("favorites.json"),
        )));
        favorites.lock().unwrap().add(FavoriteEntry {
            uri: "local:Artist - Song.mp3".to_string(),
            name: "Song".to_string(),
            artist: "Artist".to_string(),
            album: String::new(),
            cover_url: String::new(),
            source: FavoriteSource::LocalImport,
            file_path: Some(mp3.to_string_lossy().to_string()),
            cover_path: None,
            duration_ms: None,
            spotify_duration_ms: None,
            downloaded: true,
            added_at: "0".to_string(),
        });

        assert_eq!(sync_existing_music_covers(&favorites), 0);
        let linked_cover = favorites
            .lock()
            .unwrap()
            .find_by_uri("local:Artist - Song.mp3")
            .and_then(|entry| entry.cover_path.clone());
        assert_eq!(linked_cover, None);
        assert!(cover.exists());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn existing_spotify_cover_backfill_uses_cover_url() {
        let entry = FavoriteEntry {
            uri: "spotify:track:test".to_string(),
            name: "Song".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            cover_url: "https://i.scdn.co/image/example".to_string(),
            source: FavoriteSource::Spotify,
            file_path: Some("/tmp/Artist - Song.mp3".to_string()),
            cover_path: None,
            duration_ms: Some(1234),
            spotify_duration_ms: Some(1234),
            downloaded: true,
            added_at: "0".to_string(),
        };

        assert_eq!(
            existing_online_cover_source(&entry),
            Some(ExistingOnlineCoverSource::DirectUrl(
                "https://i.scdn.co/image/example".to_string()
            ))
        );
    }

    #[test]
    fn existing_local_cover_backfill_uses_itunes_metadata() {
        let entry = FavoriteEntry {
            uri: "local:Wazi Sleeps - Twinkle, Twinkle, Little Star.mp3".to_string(),
            name: "Twinkle, Twinkle, Little Star".to_string(),
            artist: "Wazi Sleeps".to_string(),
            album: "Twinkle, Twinkle, Little Star".to_string(),
            cover_url: String::new(),
            source: FavoriteSource::LocalImport,
            file_path: Some("/tmp/Wazi Sleeps - Twinkle, Twinkle, Little Star.mp3".to_string()),
            cover_path: None,
            duration_ms: Some(126093),
            spotify_duration_ms: None,
            downloaded: true,
            added_at: "0".to_string(),
        };

        assert_eq!(
            existing_online_cover_source(&entry),
            Some(ExistingOnlineCoverSource::Itunes(ImportMetadata {
                title: "Twinkle, Twinkle, Little Star".to_string(),
                artist: "Wazi Sleeps".to_string(),
                album: "Twinkle, Twinkle, Little Star".to_string(),
                duration_ms: Some(126093),
            }))
        );
    }

    #[test]
    fn existing_online_cover_backfill_attempts_once_per_uri() {
        let uri = format!(
            "local:test-once-{}.mp3",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        assert!(should_attempt_existing_online_cover(&uri));
        assert!(!should_attempt_existing_online_cover(&uri));
    }

    #[test]
    fn sync_existing_music_covers_links_same_name_cover_in_music_dir() {
        let base = std::env::temp_dir().join(format!(
            "sideb-cover-sync-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let music_dir = base.join("music");
        fs::create_dir_all(&music_dir).unwrap();
        let mp3 = music_dir.join("Artist - Song.mp3");
        let cover = music_dir.join("Artist - Song.jpg");
        fs::write(&mp3, b"mp3").unwrap();
        fs::write(&cover, b"jpg").unwrap();

        let favorites = Arc::new(Mutex::new(FavoritesManager::load(
            base.join("favorites.json"),
        )));
        favorites.lock().unwrap().add(FavoriteEntry {
            uri: "local:Artist - Song.mp3".to_string(),
            name: "Song".to_string(),
            artist: "Artist".to_string(),
            album: String::new(),
            cover_url: String::new(),
            source: FavoriteSource::LocalImport,
            file_path: Some(mp3.to_string_lossy().to_string()),
            cover_path: None,
            duration_ms: None,
            spotify_duration_ms: None,
            downloaded: true,
            added_at: "0".to_string(),
        });

        assert_eq!(sync_existing_music_covers(&favorites), 1);
        let linked_cover = favorites
            .lock()
            .unwrap()
            .find_by_uri("local:Artist - Song.mp3")
            .and_then(|entry| entry.cover_path.clone());
        assert_eq!(linked_cover, Some(cover.to_string_lossy().to_string()));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn embedded_cover_extraction_uses_system_ffmpeg() {
        assert_eq!(embedded_cover_extractor_bin(), "/usr/bin/ffmpeg");
    }

    #[test]
    fn resolve_metadata_reports_filename_fallback() {
        let (meta, source) = resolve_metadata(Path::new("/tmp/missing.mp3"), "Artist - Song");
        assert_eq!(source, MetadataSource::Filename);
        assert_eq!(meta.artist, "Artist");
        assert_eq!(meta.title, "Song");
    }

    #[test]
    fn itunes_artwork_parser_prefers_matching_track_result() {
        let metadata = ImportMetadata {
            title: "Sakura Nagashi".to_string(),
            artist: "Utada Hikaru".to_string(),
            album: String::new(),
            duration_ms: None,
        };
        let json = r#"{
          "resultCount": 2,
          "results": [
            {
              "artistName": "Different Artist",
              "trackName": "Sakura Nagashi",
              "collectionName": "Other Album",
              "artworkUrl100": "https://example.test/wrong/100x100bb.jpg"
            },
            {
              "artistName": "Utada Hikaru",
              "trackName": "Sakura Nagashi",
              "collectionName": "Evangelion",
              "artworkUrl100": "https://example.test/right/100x100bb.jpg"
            }
          ]
        }"#;

        assert_eq!(
            select_itunes_artwork_url(json, &metadata).as_deref(),
            Some("https://example.test/right/600x600bb.jpg")
        );
    }

    #[test]
    fn itunes_search_url_encodes_artist_title_and_album() {
        let metadata = ImportMetadata {
            title: "Komm, Susser Tod".to_string(),
            artist: "Arianne".to_string(),
            album: "THE END OF EVANGELION".to_string(),
            duration_ms: None,
        };

        let url = build_itunes_search_url(&metadata);

        assert!(url.contains("media=music"));
        assert!(url.contains("entity=song"));
        assert!(url.contains("limit=5"));
        assert!(url.contains("term=Arianne+Komm%2C+Susser+Tod+THE+END+OF+EVANGELION"));
    }
}
