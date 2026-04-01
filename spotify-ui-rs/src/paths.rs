use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub app_dir: PathBuf,
    pub data_dir: PathBuf,
    pub resources_dir: PathBuf,
    pub imports_dir: PathBuf,
    pub music_dir: PathBuf,
    pub favorites_path: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct PathOverrides {
    app_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    resources_dir: Option<PathBuf>,
}

pub const SIDEB_APP_DIR_ENV: &str = "SIDEB_APP_DIR";
pub const SIDEB_DATA_DIR_ENV: &str = "SIDEB_DATA_DIR";
pub const SIDEB_RESOURCES_DIR_ENV: &str = "SIDEB_RESOURCES_DIR";

static APP_PATHS: OnceLock<AppPaths> = OnceLock::new();

pub fn detect_paths() -> AppPaths {
    detect_paths_with(PathOverrides {
        app_dir: env_path(SIDEB_APP_DIR_ENV),
        data_dir: env_path(SIDEB_DATA_DIR_ENV),
        resources_dir: env_path(SIDEB_RESOURCES_DIR_ENV),
    })
}

pub fn init_paths(paths: AppPaths) {
    let _ = APP_PATHS.set(paths);
}

pub fn app_paths() -> &'static AppPaths {
    APP_PATHS.get_or_init(detect_paths)
}

fn detect_paths_with(overrides: PathOverrides) -> AppPaths {
    let app_dir = overrides.app_dir.unwrap_or_else(detect_base_dir);
    let data_dir = overrides.data_dir.unwrap_or_else(|| app_dir.join("data"));
    let resources_dir = overrides
        .resources_dir
        .unwrap_or_else(|| app_dir.join("resources"));

    AppPaths {
        app_dir: app_dir.clone(),
        data_dir: data_dir.clone(),
        resources_dir,
        imports_dir: data_dir.join("imports"),
        music_dir: data_dir.join("music"),
        favorites_path: data_dir.join("favorites.json"),
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn detect_base_dir() -> PathBuf {
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join("data").is_dir() || cwd.join("resources").is_dir() {
            return cwd;
        }

        for candidate in repo_layout_candidates(&cwd) {
            if candidate.is_dir() {
                return candidate;
            }
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            for candidate in repo_layout_candidates(parent) {
                if candidate.is_dir() {
                    return candidate;
                }
            }
            return parent.to_path_buf();
        }
    }

    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn repo_layout_candidates(base: &std::path::Path) -> [PathBuf; 4] {
    [
        base.join("package/SideB.pak"),
        base.join("package/SideB"),
        base.join("../package/SideB.pak"),
        base.join("../package/SideB"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_paths_defaults_to_app_relative_layout() {
        let paths = detect_paths_with(PathOverrides {
            app_dir: Some(PathBuf::from("/sdcard/Tools/tg5040/SideB.pak")),
            ..PathOverrides::default()
        });

        assert_eq!(
            paths.app_dir,
            PathBuf::from("/sdcard/Tools/tg5040/SideB.pak")
        );
        assert_eq!(
            paths.data_dir,
            PathBuf::from("/sdcard/Tools/tg5040/SideB.pak/data")
        );
        assert_eq!(
            paths.resources_dir,
            PathBuf::from("/sdcard/Tools/tg5040/SideB.pak/resources")
        );
        assert_eq!(
            paths.imports_dir,
            PathBuf::from("/sdcard/Tools/tg5040/SideB.pak/data/imports")
        );
        assert_eq!(
            paths.music_dir,
            PathBuf::from("/sdcard/Tools/tg5040/SideB.pak/data/music")
        );
        assert_eq!(
            paths.favorites_path,
            PathBuf::from("/sdcard/Tools/tg5040/SideB.pak/data/favorites.json")
        );
    }

    #[test]
    fn detect_paths_honors_explicit_data_and_resources_overrides() {
        let paths = detect_paths_with(PathOverrides {
            app_dir: Some(PathBuf::from("/apps/SideB")),
            data_dir: Some(PathBuf::from("/mnt/userdata/sideb-data")),
            resources_dir: Some(PathBuf::from("/mnt/userdata/sideb-assets")),
        });

        assert_eq!(paths.app_dir, PathBuf::from("/apps/SideB"));
        assert_eq!(paths.data_dir, PathBuf::from("/mnt/userdata/sideb-data"));
        assert_eq!(
            paths.resources_dir,
            PathBuf::from("/mnt/userdata/sideb-assets")
        );
        assert_eq!(
            paths.imports_dir,
            PathBuf::from("/mnt/userdata/sideb-data/imports")
        );
        assert_eq!(
            paths.music_dir,
            PathBuf::from("/mnt/userdata/sideb-data/music")
        );
        assert_eq!(
            paths.favorites_path,
            PathBuf::from("/mnt/userdata/sideb-data/favorites.json")
        );
    }
}
