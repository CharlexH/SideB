use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;

use crate::constants::SYSTEM_FFMPEG_BIN;
use crate::favorites::{FavoriteEntry, FavoriteSource, FavoritesManager};
use crate::mode::InputAction;
use crate::paths::app_paths;

const IMPORT_SCAN_INTERVAL: Duration = Duration::from_secs(3);

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
    let imports_dir = app_paths().imports_dir.clone();
    let music_dir = app_paths().music_dir.clone();
    let _ = fs::create_dir_all(&imports_dir);
    let _ = fs::create_dir_all(&music_dir);

    let entries = match fs::read_dir(&imports_dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("import: read_dir failed for {}: {e}", imports_dir.display());
            return 0;
        }
    };

    let mut import_candidates = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_mp3 = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("mp3"))
            .unwrap_or(false);
        if !is_mp3 {
            continue;
        }

        import_candidates.push(path);
    }

    if !import_candidates.is_empty() {
        eprintln!(
            "import: found {} candidate mp3 file(s) in {}",
            import_candidates.len(),
            imports_dir.display()
        );
    }

    let mut imported = 0usize;
    for path in import_candidates {
        match import_one(&path, &music_dir) {
            Ok(entry) => {
                favorites.lock().unwrap().add(entry);
                imported += 1;
            }
            Err(e) => {
                eprintln!("import: {}: {e}", path.display());
            }
        }
    }

    imported
}

pub fn run(
    favorites: Arc<Mutex<FavoritesManager>>,
    cmd_tx: Sender<InputAction>,
    quit: Arc<AtomicBool>,
) {
    let _ = fs::create_dir_all(&app_paths().imports_dir);
    while !quit.load(Ordering::Relaxed) {
        let imported = scan_once(&favorites);
        if imported > 0 {
            eprintln!("import: added {imported} local track(s)");
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

fn import_one(import_mp3: &Path, music_dir: &Path) -> Result<FavoriteEntry, String> {
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
    } else {
        eprintln!("import: no cover found for {}", target_mp3.display());
        None
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
    for ext in ["jpg", "jpeg", "png"] {
        let candidate = parent.join(format!("{stem}.{ext}"));
        if candidate.exists() {
            return Some(candidate);
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
}
