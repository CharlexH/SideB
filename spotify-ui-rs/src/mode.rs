/// Application mode — determines UI rendering and input routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppMode {
    /// No Spotify connection, no local playback. Shows waiting screen.
    #[default]
    Waiting,
    /// Spotify Connect active.
    Spotify,
    /// Playing local downloaded tracks.
    Local,
}

/// Actions dispatched from input/network threads to the command processor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    ToggleFavorite,
    TogglePlayPause,
    NextTrack,
    PrevTrack,
    VolumeUp,
    VolumeDown,
    StartLocalPlayback,
    StopLocalPlayback,
    TogglePlaylist,
    PlaylistUp,
    PlaylistDown,
    PlaylistSelect,
    PlaylistDelete,
    LibraryChanged,
    ImportProgress { completed: usize, total: usize },
    ImportFinished { failed: usize },
    SpotifyActivated,
    SpotifyDeactivated,
    SpotifyTrackChanged,
    LockScreen,
    UnlockScreen,
    RequestExit,
    ExitApp,
}
