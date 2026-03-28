/// Application mode — determines UI rendering and input routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    /// No Spotify connection, no local playback. Shows waiting screen.
    Waiting,
    /// Spotify Connect active.
    Spotify,
    /// Playing local downloaded tracks.
    Local,
}

impl Default for AppMode {
    fn default() -> Self {
        Self::Waiting
    }
}

/// Actions dispatched from input/network threads to the command processor.
#[derive(Debug)]
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
    SpotifyActivated,
    SpotifyDeactivated,
    SpotifyTrackChanged,
    RequestExit,
    ExitApp,
}
