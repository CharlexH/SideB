use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FavoriteEntry {
    pub uri: String,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub cover_url: String,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub cover_path: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub downloaded: bool,
    pub added_at: String,
}

pub struct FavoritesManager {
    entries: Vec<FavoriteEntry>,
    path: PathBuf,
}

impl FavoritesManager {
    /// Load favorites from JSON file. Returns empty list if file is missing or corrupt.
    pub fn load(path: &str) -> Self {
        let path = PathBuf::from(path);
        let entries = if path.exists() {
            match fs::read_to_string(&path) {
                Ok(data) => match serde_json::from_str::<Vec<FavoriteEntry>>(&data) {
                    Ok(entries) => {
                        eprintln!("favorites: loaded {} entries", entries.len());
                        entries
                    }
                    Err(e) => {
                        eprintln!("favorites: parse error: {e}");
                        Vec::new()
                    }
                },
                Err(e) => {
                    eprintln!("favorites: read error: {e}");
                    Vec::new()
                }
            }
        } else {
            eprintln!("favorites: no file at {}, starting empty", path.display());
            Vec::new()
        };

        Self { entries, path }
    }

    /// Atomic save: write to .tmp, then rename over original.
    pub fn save(&self) {
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
        // Don't add duplicates
        if self.entries.iter().any(|e| e.uri == entry.uri) {
            return;
        }
        eprintln!("favorites: adding {} - {}", entry.artist, entry.name);
        self.entries.push(entry);
        self.save();
    }

    /// Remove a favorite by URI. Deletes associated files. Saves immediately.
    /// Returns the removed entry if found.
    pub fn remove(&mut self, uri: &str) -> Option<FavoriteEntry> {
        let idx = self.entries.iter().position(|e| e.uri == uri)?;
        let entry = self.entries.remove(idx);

        // Delete MP3 file
        if let Some(ref fp) = entry.file_path {
            if Path::new(fp).exists() {
                if let Err(e) = fs::remove_file(fp) {
                    eprintln!("favorites: delete mp3 error: {e}");
                }
            }
        }

        // Delete cover file
        if let Some(ref cp) = entry.cover_path {
            if Path::new(cp).exists() {
                let _ = fs::remove_file(cp);
            }
        }

        eprintln!("favorites: removed {} - {}", entry.artist, entry.name);
        self.save();
        Some(entry)
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
            eprintln!("favorites: marked downloaded {} - {}", entry.artist, entry.name);
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
        let mut files = std::collections::HashSet::new();
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
}
