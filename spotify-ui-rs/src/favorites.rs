use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FavoriteSource {
    #[default]
    Spotify,
    LocalImport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FavoriteEntry {
    pub uri: String,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub cover_url: String,
    #[serde(default)]
    pub source: FavoriteSource,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub cover_path: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub spotify_duration_ms: Option<i64>,
    #[serde(default)]
    pub downloaded: bool,
    pub added_at: String,
}

pub struct FavoritesManager {
    entries: Vec<FavoriteEntry>,
    path: PathBuf,
    load_state: FavoritesLoadState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FavoritesLoadState {
    Loaded,
    Missing,
    ReadError,
    ParseError,
}

impl FavoritesManager {
    /// Load favorites from JSON file. Missing files start a new clean library;
    /// read/parse failures keep destructive cleanup and saves disabled.
    pub fn load(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        let (entries, load_state) = if path.exists() {
            match fs::read_to_string(&path) {
                Ok(data) => match serde_json::from_str::<Vec<FavoriteEntry>>(&data) {
                    Ok(entries) => {
                        eprintln!("favorites: loaded {} entries", entries.len());
                        (entries, FavoritesLoadState::Loaded)
                    }
                    Err(e) => {
                        eprintln!("favorites: parse error: {e}");
                        (Vec::new(), FavoritesLoadState::ParseError)
                    }
                },
                Err(e) => {
                    eprintln!("favorites: read error: {e}");
                    (Vec::new(), FavoritesLoadState::ReadError)
                }
            }
        } else {
            eprintln!("favorites: no file at {}, starting empty", path.display());
            (Vec::new(), FavoritesLoadState::Missing)
        };

        Self {
            entries,
            path,
            load_state,
        }
    }

    /// Atomic save: write to .tmp, then rename over original.
    pub fn save(&self) {
        if !self.allows_save() {
            eprintln!(
                "favorites: refusing to save because {} failed to load cleanly",
                self.path.display()
            );
            return;
        }

        let tmp_path = self.path.with_extension("json.tmp");
        match serde_json::to_string_pretty(&self.entries) {
            Ok(json) => {
                if let Some(parent) = self.path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if let Err(e) = fs::write(&tmp_path, &json) {
                    eprintln!("favorites: write tmp error: {e}");
                    return;
                }
                if let Err(e) = fs::rename(&tmp_path, &self.path) {
                    eprintln!("favorites: rename error: {e}");
                }
            }
            Err(e) => {
                eprintln!("favorites: serialize error: {e}");
            }
        }
    }

    /// Add a new favorite entry. Saves immediately.
    pub fn add(&mut self, entry: FavoriteEntry) {
        if self.entries.iter().any(|e| e.uri == entry.uri) {
            return;
        }
        eprintln!("favorites: adding {} - {}", entry.artist, entry.name);
        self.entries.push(entry);
        self.save();
    }

    /// Remove a favorite by URI without deleting associated files. Saves immediately.
    /// Returns the removed entry if found.
    pub fn remove_preserving_files(&mut self, uri: &str) -> Option<FavoriteEntry> {
        let idx = self.entries.iter().position(|e| e.uri == uri)?;
        let entry = self.entries.remove(idx);

        eprintln!("favorites: removed {} - {}", entry.artist, entry.name);
        self.save();
        Some(entry)
    }

    /// Remove a favorite by URI. Deletes associated files. Saves immediately.
    /// Returns the removed entry if found.
    pub fn remove(&mut self, uri: &str) -> Option<FavoriteEntry> {
        let entry = self.remove_preserving_files(uri)?;
        Self::delete_entry_files(&entry);
        Some(entry)
    }

    pub fn delete_entry_files(entry: &FavoriteEntry) {
        Self::delete_entry_files_under(entry, &crate::paths::app_paths().music_dir);
    }

    pub fn delete_entry_files_under(entry: &FavoriteEntry, managed_dir: &Path) {
        let managed_root = match managed_dir.canonicalize() {
            Ok(path) => path,
            Err(e) => {
                eprintln!(
                    "favorites: skip file deletion because managed dir {} is unavailable: {e}",
                    managed_dir.display()
                );
                return;
            }
        };

        if let Some(ref fp) = entry.file_path {
            if let Some(path) = managed_existing_file(Path::new(fp), &managed_root) {
                if let Err(e) = fs::remove_file(&path) {
                    eprintln!("favorites: delete mp3 error: {e}");
                }
            }
        }

        if let Some(ref cp) = entry.cover_path {
            if let Some(path) = managed_existing_file(Path::new(cp), &managed_root) {
                let _ = fs::remove_file(path);
            }
        }
    }

    pub fn allows_save(&self) -> bool {
        matches!(
            self.load_state,
            FavoritesLoadState::Loaded | FavoritesLoadState::Missing
        )
    }

    pub fn allows_destructive_cleanup(&self) -> bool {
        self.allows_save()
    }

    pub fn is_favorited(&self, uri: &str) -> bool {
        self.entries.iter().any(|e| e.uri == uri)
    }

    pub fn find_by_uri(&self, uri: &str) -> Option<&FavoriteEntry> {
        self.entries.iter().find(|e| e.uri == uri)
    }

    /// Mark a track as downloaded with its file path and duration.
    pub fn mark_downloaded(&mut self, uri: &str, file_path: &str, duration_ms: Option<i64>) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.uri == uri) {
            entry.downloaded = true;
            entry.file_path = Some(file_path.to_string());
            entry.duration_ms = duration_ms;
            eprintln!(
                "favorites: marked downloaded {} - {}",
                entry.artist, entry.name
            );
            self.save();
        }
    }

    /// Set the cover path for a track.
    pub fn set_cover_path(&mut self, uri: &str, cover_path: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.uri == uri) {
            entry.cover_path = Some(cover_path.to_string());
            self.save();
        }
    }

    /// Return all entries that have been downloaded.
    pub fn downloaded_entries(&self) -> Vec<FavoriteEntry> {
        self.entries
            .iter()
            .filter(|e| e.downloaded && e.file_path.is_some())
            .cloned()
            .collect()
    }

    pub fn all_entries(&self) -> &[FavoriteEntry] {
        &self.entries
    }

    pub fn count(&self) -> usize {
        self.entries.len()
    }

    pub fn downloaded_count(&self) -> usize {
        self.entries.iter().filter(|e| e.downloaded).count()
    }

    /// Return all file paths referenced by favorites (MP3 + cover).
    pub fn referenced_files(&self) -> std::collections::HashSet<String> {
        let mut files = HashSet::new();
        for entry in &self.entries {
            if let Some(ref fp) = entry.file_path {
                files.insert(fp.clone());
            }
            if let Some(ref cp) = entry.cover_path {
                files.insert(cp.clone());
            }
        }
        files
    }

    pub fn referenced_managed_files(&self, managed_dir: &Path) -> HashSet<PathBuf> {
        let managed_root = match managed_dir.canonicalize() {
            Ok(path) => path,
            Err(e) => {
                eprintln!(
                    "favorites: skip referenced file scan because managed dir {} is unavailable: {e}",
                    managed_dir.display()
                );
                return HashSet::new();
            }
        };

        let mut files = HashSet::new();
        for entry in &self.entries {
            if let Some(ref fp) = entry.file_path {
                if let Some(path) = managed_existing_file(Path::new(fp), &managed_root) {
                    files.insert(path);
                }
            }
            if let Some(ref cp) = entry.cover_path {
                if let Some(path) = managed_existing_file(Path::new(cp), &managed_root) {
                    files.insert(path);
                }
            }
        }
        files
    }
}

fn managed_existing_file(path: &Path, managed_root: &Path) -> Option<PathBuf> {
    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(_) => return None,
    };

    if canonical.starts_with(managed_root) && canonical.is_file() {
        Some(canonical)
    } else {
        eprintln!(
            "favorites: skip unmanaged file path {}",
            canonical.display()
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_entry(
        uri: &str,
        file_path: Option<String>,
        cover_path: Option<String>,
    ) -> FavoriteEntry {
        FavoriteEntry {
            uri: uri.to_string(),
            name: "Track".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            cover_url: String::new(),
            source: FavoriteSource::Spotify,
            file_path,
            cover_path,
            duration_ms: None,
            spotify_duration_ms: None,
            downloaded: true,
            added_at: "0".to_string(),
        }
    }

    fn unique_test_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "sideb-favorites-test-{}-{unique}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn remove_preserving_files_keeps_managed_files_on_disk() {
        let dir = unique_test_dir();
        fs::create_dir_all(&dir).unwrap();
        let favorites_path = dir.join("favorites.json");
        let mp3_path = dir.join("track.mp3");
        let cover_path = dir.join("track.jpg");
        fs::write(&mp3_path, b"mp3").unwrap();
        fs::write(&cover_path, b"jpg").unwrap();

        let mut favorites = FavoritesManager {
            entries: vec![test_entry(
                "track:1",
                Some(mp3_path.to_string_lossy().to_string()),
                Some(cover_path.to_string_lossy().to_string()),
            )],
            path: favorites_path,
            load_state: FavoritesLoadState::Loaded,
        };

        let removed = favorites.remove_preserving_files("track:1");

        assert!(removed.is_some());
        assert!(mp3_path.exists());
        assert!(cover_path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_entry_files_removes_existing_mp3_and_cover() {
        let dir = unique_test_dir();
        let music_dir = dir.join("music");
        fs::create_dir_all(&music_dir).unwrap();
        let mp3_path = music_dir.join("track.mp3");
        let cover_path = music_dir.join("track.jpg");
        fs::write(&mp3_path, b"mp3").unwrap();
        fs::write(&cover_path, b"jpg").unwrap();

        let entry = test_entry(
            "track:1",
            Some(mp3_path.to_string_lossy().to_string()),
            Some(cover_path.to_string_lossy().to_string()),
        );

        FavoritesManager::delete_entry_files_under(&entry, &music_dir);

        assert!(!mp3_path.exists());
        assert!(!cover_path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_parse_error_blocks_destructive_cleanup_and_save() {
        let dir = unique_test_dir();
        fs::create_dir_all(&dir).unwrap();
        let favorites_path = dir.join("favorites.json");
        fs::write(&favorites_path, b"{not valid json").unwrap();

        let favorites = FavoritesManager::load(&favorites_path);

        assert_eq!(favorites.count(), 0);
        assert!(!favorites.allows_destructive_cleanup());
        assert!(!favorites.allows_save());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_favorites_file_allows_destructive_cleanup_and_save() {
        let dir = unique_test_dir();
        let favorites_path = dir.join("favorites.json");

        let favorites = FavoritesManager::load(&favorites_path);

        assert_eq!(favorites.count(), 0);
        assert!(favorites.allows_destructive_cleanup());
        assert!(favorites.allows_save());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_entry_files_under_skips_paths_outside_managed_dir() {
        let dir = unique_test_dir();
        let music_dir = dir.join("music");
        let outside_dir = dir.join("outside");
        fs::create_dir_all(&music_dir).unwrap();
        fs::create_dir_all(&outside_dir).unwrap();
        let outside_mp3 = outside_dir.join("track.mp3");
        let outside_cover = outside_dir.join("track.jpg");
        fs::write(&outside_mp3, b"mp3").unwrap();
        fs::write(&outside_cover, b"jpg").unwrap();

        let entry = test_entry(
            "track:1",
            Some(outside_mp3.to_string_lossy().to_string()),
            Some(outside_cover.to_string_lossy().to_string()),
        );

        FavoritesManager::delete_entry_files_under(&entry, &music_dir);

        assert!(outside_mp3.exists());
        assert!(outside_cover.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn referenced_managed_files_ignores_unmanaged_paths() {
        let dir = unique_test_dir();
        let music_dir = dir.join("music");
        let outside_dir = dir.join("outside");
        fs::create_dir_all(&music_dir).unwrap();
        fs::create_dir_all(&outside_dir).unwrap();
        let managed_mp3 = music_dir.join("track.mp3");
        let outside_cover = outside_dir.join("track.jpg");
        fs::write(&managed_mp3, b"mp3").unwrap();
        fs::write(&outside_cover, b"jpg").unwrap();

        let favorites = FavoritesManager {
            entries: vec![test_entry(
                "track:1",
                Some(managed_mp3.to_string_lossy().to_string()),
                Some(outside_cover.to_string_lossy().to_string()),
            )],
            path: dir.join("favorites.json"),
            load_state: FavoritesLoadState::Loaded,
        };

        let referenced = favorites.referenced_managed_files(&music_dir);

        assert!(referenced.contains(&managed_mp3.canonicalize().unwrap()));
        assert!(!referenced.contains(&outside_cover.canonicalize().unwrap()));

        let _ = fs::remove_dir_all(&dir);
    }
}
