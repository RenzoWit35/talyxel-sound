use crate::spotify::{Device, PlaybackState, Playlist, Track};

/// Which list the track pane is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Empty,
    Liked,
    Recent,
    Search(String),
    Playlist {
        id: String,
        uri: String,
        name: String,
    },
}

impl View {
    pub fn title(&self) -> String {
        match self {
            View::Empty => "Tracks".into(),
            View::Liked => "Liked Songs".into(),
            View::Recent => "Recently Played".into(),
            View::Search(q) => format!("Search: {q}"),
            View::Playlist { name, .. } => name.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MediaCommand {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
}

/// Events produced by background tasks and delivered to the UI loop.
pub enum AppEvent {
    /// Remote playback state changed (from the Web API poller).
    Remote(Option<PlaybackState>),
    /// Initial playlist load or a detected library change.
    Playlists {
        playlists: Vec<Playlist>,
        changed: Vec<String>,
        initial: bool,
    },
    /// Liked Songs changed on another device.
    LikedChanged,
    Tracks {
        view: View,
        tracks: Vec<Track>,
    },
    Devices(Vec<Device>),
    Media(MediaCommand),
    UpdateAvailable(String),
    UpdateInstalled(String),
    Status(String),
    Error(String),
}
